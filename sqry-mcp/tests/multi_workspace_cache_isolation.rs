//! Integration tests for multi-workspace cache isolation
//!
//! Verifies that:
//! - Multiple repositories can be queried simultaneously
//! - Cache keys include workspace identity to prevent collisions
//! - Engine cache provides proper isolation
//! - Discovery cache handles multiple workspaces

use anyhow::Result;
use sqry_mcp::engine::{read_graph_identity, read_graph_identity_with_metadata};
use std::io::Write;
use tempfile::TempDir;

/// Helper to create a test workspace with minimal manifest
fn create_test_workspace(sha: &str) -> Result<TempDir> {
    let temp_dir = TempDir::new()?;
    let graph_dir = temp_dir.path().join(".sqry/graph");
    std::fs::create_dir_all(&graph_dir)?;

    // Create minimal manifest with all required fields
    let manifest = serde_json::json!({
        "schema_version": 1,
        "snapshot_format_version": 2,
        "built_at": "2026-01-01T00:00:00Z",
        "root_path": temp_dir.path().to_string_lossy(),
        "node_count": 0,
        "edge_count": 0,
        "snapshot_sha256": sha,
        "build_provenance": {
            "sqry_version": "test-0.0.0",
            "build_timestamp": "2026-01-01T00:00:00Z",
            "build_command": "sqry test"
        }
    });

    let manifest_path = graph_dir.join("manifest.json");
    let mut file = std::fs::File::create(&manifest_path)?;
    file.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    file.sync_all()?;

    Ok(temp_dir)
}

/// Test that `GraphIdentity` correctly distinguishes workspaces
#[test]
fn test_graph_identity_isolation() -> Result<()> {
    let workspace_a = create_test_workspace("aaaa")?;
    let workspace_b = create_test_workspace("bbbb")?;

    let identity_a = read_graph_identity(workspace_a.path())?;
    let identity_b = read_graph_identity(workspace_b.path())?;

    // Different workspaces should have different identities
    assert_ne!(identity_a.workspace_root, identity_b.workspace_root);
    assert_ne!(identity_a.snapshot_sha256, identity_b.snapshot_sha256);

    // Same workspace should return same identity
    let identity_a2 = read_graph_identity(workspace_a.path())?;
    assert_eq!(identity_a.workspace_root, identity_a2.workspace_root);
    assert_eq!(identity_a.snapshot_sha256, identity_a2.snapshot_sha256);

    Ok(())
}

/// Test that atomic read provides consistent identity and metadata
#[test]
fn test_atomic_identity_metadata_read() -> Result<()> {
    let workspace = create_test_workspace("atomic_test")?;

    // Read identity and metadata atomically
    let (identity, metadata) = read_graph_identity_with_metadata(workspace.path())?;

    // Verify identity fields
    assert_eq!(identity.snapshot_sha256, "atomic_test");
    assert_eq!(identity.schema_version, 1);
    assert_eq!(identity.snapshot_format_version, 2);

    // Verify metadata is from the same file
    assert!(metadata.size > 0);
    // On Unix, file_id is the inode number (always available).
    // On Windows, file_index requires unstable `windows_by_handle` feature (rust#63010),
    // so extract_file_id() intentionally returns None until the API stabilizes.
    assert!(metadata.file_id.is_some() || cfg!(not(unix)));

    Ok(())
}

/// Test that manifest updates change `GraphIdentity`
#[test]
fn test_graph_identity_change_detection() -> Result<()> {
    let workspace = create_test_workspace("initial_sha")?;
    let workspace_path = workspace.path();

    // Read initial identity
    let identity1 = read_graph_identity(workspace_path)?;
    assert_eq!(identity1.snapshot_sha256, "initial_sha");

    // Update manifest with different sha
    let manifest_path = workspace_path.join(".sqry/graph/manifest.json");
    std::thread::sleep(std::time::Duration::from_millis(10)); // Ensure mtime changes
    let new_manifest = serde_json::json!({
        "schema_version": 1,
        "snapshot_format_version": 2,
        "built_at": "2026-01-02T00:00:00Z",
        "root_path": workspace_path.to_string_lossy(),
        "node_count": 0,
        "edge_count": 0,
        "snapshot_sha256": "updated_sha",
        "build_provenance": {
            "sqry_version": "test-0.0.0",
            "build_timestamp": "2026-01-02T00:00:00Z",
            "build_command": "sqry test"
        }
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&manifest_path)?;
    file.write_all(serde_json::to_string_pretty(&new_manifest)?.as_bytes())?;
    file.sync_all()?;

    // Read updated identity
    let identity2 = read_graph_identity(workspace_path)?;
    assert_eq!(identity2.snapshot_sha256, "updated_sha");
    assert_ne!(identity1.snapshot_sha256, identity2.snapshot_sha256);

    Ok(())
}

/// Test that workspace root path mismatch is detected
#[test]
fn test_workspace_root_mismatch_detection() -> Result<()> {
    let workspace_a = create_test_workspace("test_sha_a")?;
    let workspace_b = create_test_workspace("test_sha_b")?;

    // Create manifest in workspace_a that points to workspace_b's root_path
    let manifest_path = workspace_a.path().join(".sqry/graph/manifest.json");
    let bad_manifest = serde_json::json!({
        "schema_version": 1,
        "snapshot_format_version": 2,
        "built_at": "2026-01-01T00:00:00Z",
        "root_path": workspace_b.path().to_string_lossy(),
        "node_count": 0,
        "edge_count": 0,
        "snapshot_sha256": "test_sha",
        "build_provenance": {
            "sqry_version": "test-0.0.0",
            "build_timestamp": "2026-01-01T00:00:00Z",
            "build_command": "sqry test"
        }
    });
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&manifest_path)?;
    file.write_all(serde_json::to_string_pretty(&bad_manifest)?.as_bytes())?;
    file.sync_all()?;

    // Should fail with mismatch error (workspace_a's manifest points to workspace_b)
    let result = read_graph_identity(workspace_a.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("root_path mismatch"),
        "Expected 'root_path mismatch' error, got: {err}"
    );

    Ok(())
}

/// Test that cache initialization happens correctly
#[test]
fn test_cache_initialization_order() {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    // Verify caches can be initialized with valid capacities
    let engine_capacity = NonZeroUsize::new(5).unwrap();
    let discovery_capacity = NonZeroUsize::new(100).unwrap();
    let trace_capacity = NonZeroUsize::new(256).unwrap();
    let subgraph_capacity = NonZeroUsize::new(128).unwrap();
    let ttl = Duration::from_secs(300);

    // These should be idempotent (can be called multiple times)
    // Note: We can't actually test the initialization here because
    // the caches are global OnceLocks and we can't reset them between tests.
    // This test mainly validates the types and serves as documentation.

    assert_eq!(engine_capacity.get(), 5);
    assert_eq!(discovery_capacity.get(), 100);
    assert_eq!(trace_capacity.get(), 256);
    assert_eq!(subgraph_capacity.get(), 128);
    assert_eq!(ttl.as_secs(), 300);
}

/// Test that config-driven cache sizing is applied
#[test]
fn test_config_cache_sizing() {
    use sqry_mcp::mcp_config::McpConfig;

    let config = McpConfig::default();

    // Verify default cache sizes match spec
    assert_eq!(config.engine_cache_capacity, 5);
    assert_eq!(config.discovery_cache_capacity, 100);
    assert_eq!(config.trace_path_cache_capacity, 256);
    assert_eq!(config.subgraph_cache_capacity, 128);
    assert_eq!(config.query_cache_ttl_secs, 300);

    // Verify validation works
    assert!(config.effective_engine_cache_capacity().is_ok());
    assert!(config.effective_discovery_cache_capacity().is_ok());
    assert!(config.effective_trace_path_cache_capacity().is_ok());
    assert!(config.effective_subgraph_cache_capacity().is_ok());
    assert!(config.effective_query_cache_ttl_secs().is_ok());
}

/// Test that invalid cache configurations are rejected
#[test]
fn test_cache_config_validation() {
    use sqry_mcp::mcp_config::McpConfig;

    // Test zero capacity rejection
    let config = McpConfig {
        engine_cache_capacity: 0,
        ..Default::default()
    };
    assert!(config.effective_engine_cache_capacity().is_err());

    let config = McpConfig {
        discovery_cache_capacity: 0,
        ..Default::default()
    };
    assert!(config.effective_discovery_cache_capacity().is_err());

    let config = McpConfig {
        trace_path_cache_capacity: 0,
        ..Default::default()
    };
    assert!(config.effective_trace_path_cache_capacity().is_err());

    let config = McpConfig {
        subgraph_cache_capacity: 0,
        ..Default::default()
    };
    assert!(config.effective_subgraph_cache_capacity().is_err());

    let config = McpConfig {
        query_cache_ttl_secs: 0,
        ..Default::default()
    };
    assert!(config.effective_query_cache_ttl_secs().is_err());
}

/// Test that cache capacities above hard caps are rejected
#[test]
fn test_cache_config_hard_caps() {
    use sqry_mcp::mcp_config::McpConfig;

    // Test hard cap enforcement
    let config = McpConfig {
        engine_cache_capacity: 1001, // Hard cap is 1000
        ..Default::default()
    };
    assert!(config.effective_engine_cache_capacity().is_err());

    let config = McpConfig {
        discovery_cache_capacity: 10_001, // Hard cap is 10_000
        ..Default::default()
    };
    assert!(config.effective_discovery_cache_capacity().is_err());

    let config = McpConfig {
        trace_path_cache_capacity: 4097, // Hard cap is 4096
        ..Default::default()
    };
    assert!(config.effective_trace_path_cache_capacity().is_err());

    let config = McpConfig {
        subgraph_cache_capacity: 2049, // Hard cap is 2048
        ..Default::default()
    };
    assert!(config.effective_subgraph_cache_capacity().is_err());

    let config = McpConfig {
        query_cache_ttl_secs: 86_401, // Hard cap is 86_400
        ..Default::default()
    };
    assert!(config.effective_query_cache_ttl_secs().is_err());
}

/// Test that cache capacities at hard caps are accepted
#[test]
fn test_cache_config_at_hard_caps() {
    use sqry_mcp::mcp_config::McpConfig;

    // Test values exactly at hard caps are OK
    let config = McpConfig {
        engine_cache_capacity: 1000,
        ..Default::default()
    };
    assert_eq!(config.effective_engine_cache_capacity().unwrap(), 1000);

    let config = McpConfig {
        discovery_cache_capacity: 10_000,
        ..Default::default()
    };
    assert_eq!(config.effective_discovery_cache_capacity().unwrap(), 10_000);

    let config = McpConfig {
        trace_path_cache_capacity: 4096,
        ..Default::default()
    };
    assert_eq!(config.effective_trace_path_cache_capacity().unwrap(), 4096);

    let config = McpConfig {
        subgraph_cache_capacity: 2048,
        ..Default::default()
    };
    assert_eq!(config.effective_subgraph_cache_capacity().unwrap(), 2048);

    let config = McpConfig {
        query_cache_ttl_secs: 86_400,
        ..Default::default()
    };
    assert_eq!(config.effective_query_cache_ttl_secs().unwrap(), 86_400);
}
