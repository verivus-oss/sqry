//! `DoS` Prevention Tests (RR-10)
//!
//! Comprehensive tests for all denial-of-service prevention limits:
//! - Gap #1: Source file size limit
//! - Gap #2: Repository count limit
//! - Gap #3: Watch event queue capacity
//! - Gap #4: Query length and predicate limits

use serial_test::serial;
use sqry_core::config::buffers::{
    max_predicates, max_query_length, max_repositories, max_source_file_size,
    watch_event_queue_capacity,
};
use sqry_core::io::file_reader::FileReader;
use sqry_core::query::QueryParser;
use std::env;
use std::io::Write;
use tempfile::{NamedTempFile, TempDir};

/// Helper to set environment variable in test
fn set_env(key: &str, value: &str) {
    unsafe {
        env::set_var(key, value);
    }
}

/// Helper to clear environment variable
fn clear_env(key: &str) {
    unsafe {
        env::remove_var(key);
    }
}

// ============================================================================
// Gap #1: Source File Size Limit Tests
// ============================================================================

#[test]
#[serial]
fn test_file_size_within_limit() {
    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");

    // Create 1 MB file (well within 50 MB default limit)
    let mut temp = NamedTempFile::new().unwrap();
    let data = vec![b'X'; 1024 * 1024]; // 1 MB
    temp.write_all(&data).unwrap();
    temp.flush().unwrap();

    // Should succeed
    let result = FileReader::open(temp.path());
    assert!(result.is_ok(), "File within size limit should be readable");
    assert_eq!(result.unwrap().len(), 1024 * 1024);
}

#[test]
#[serial]
fn test_file_size_exceeds_default_limit() {
    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");

    // Default limit is 50 MB, create 60 MB file
    let mut temp = NamedTempFile::new().unwrap();
    let chunk_size = 1024 * 1024; // 1 MB chunks
    let num_chunks = 60; // 60 MB total
    for _ in 0..num_chunks {
        temp.write_all(&vec![b'X'; chunk_size]).unwrap();
    }
    temp.flush().unwrap();

    // Should fail with proper error message
    let result = FileReader::open(temp.path());
    assert!(result.is_err(), "File exceeding limit should be rejected");

    let error = result.err().unwrap().to_string();
    assert!(
        error.contains("File too large to index"),
        "Error should mention file too large: {error}"
    );
    assert!(
        error.contains("SQRY_MAX_SOURCE_FILE_SIZE"),
        "Error should mention environment variable: {error}"
    );
}

#[test]
#[serial]
fn test_file_size_custom_limit() {
    // Set custom limit of 2 MB
    set_env("SQRY_MAX_SOURCE_FILE_SIZE", "2097152"); // 2 MB

    // Create 3 MB file (exceeds custom limit)
    let mut temp = NamedTempFile::new().unwrap();
    let data = vec![b'X'; 3 * 1024 * 1024]; // 3 MB
    temp.write_all(&data).unwrap();
    temp.flush().unwrap();

    // Should fail with custom limit
    let result = FileReader::open(temp.path());
    assert!(result.is_err(), "File exceeding custom limit should fail");

    let error = result.err().unwrap().to_string();
    assert!(
        error.contains("exceeds 2 MB limit"),
        "Error should show custom limit: {error}"
    );

    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");
}

#[test]
#[serial]
fn test_file_size_config_function() {
    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");

    // Test default
    let default_size = max_source_file_size();
    assert_eq!(default_size, 50 * 1024 * 1024, "Default should be 50 MB");

    // Test custom value
    set_env("SQRY_MAX_SOURCE_FILE_SIZE", "104857600"); // 100 MB
    let custom_size = max_source_file_size();
    assert_eq!(
        custom_size,
        100 * 1024 * 1024,
        "Custom value should be 100 MB"
    );

    // Test clamping (below minimum)
    set_env("SQRY_MAX_SOURCE_FILE_SIZE", "100000"); // 100 KB (below 1 MB minimum)
    let clamped_low = max_source_file_size();
    assert_eq!(clamped_low, 1024 * 1024, "Should clamp to 1 MB minimum");

    // Test clamping (above maximum)
    set_env("SQRY_MAX_SOURCE_FILE_SIZE", "1073741824"); // 1 GB (above 500 MB maximum)
    let clamped_high = max_source_file_size();
    assert_eq!(
        clamped_high,
        500 * 1024 * 1024,
        "Should clamp to 500 MB maximum"
    );

    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");
}

// ============================================================================
// Gap #2: Repository Count Limit Tests
// ============================================================================

#[test]
#[serial]
fn test_repository_count_config_function() {
    clear_env("SQRY_MAX_REPOSITORIES");

    // Test default
    let default_count = max_repositories();
    assert_eq!(default_count, 1000, "Default should be 1000");

    // Test custom value
    set_env("SQRY_MAX_REPOSITORIES", "5000");
    let custom_count = max_repositories();
    assert_eq!(custom_count, 5000, "Custom value should be 5000");

    // Test clamping (below minimum)
    set_env("SQRY_MAX_REPOSITORIES", "5"); // Below 10 minimum
    let clamped_low = max_repositories();
    assert_eq!(clamped_low, 10, "Should clamp to 10 minimum");

    // Test clamping (above maximum)
    set_env("SQRY_MAX_REPOSITORIES", "20000"); // Above 10,000 maximum
    let clamped_high = max_repositories();
    assert_eq!(clamped_high, 10000, "Should clamp to 10,000 maximum");

    clear_env("SQRY_MAX_REPOSITORIES");
}

#[test]
#[serial]
fn test_repository_discovery_within_limit() {
    use sqry_core::workspace::discovery::{DiscoveryMode, discover_repositories};

    clear_env("SQRY_MAX_REPOSITORIES");

    // Create workspace with 5 repositories (well within 1000 limit)
    let workspace = TempDir::new().unwrap();
    for i in 0..5 {
        let repo_dir = workspace.path().join(format!("repo{i}"));
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(".sqry-index"), b"{}").unwrap();
    }

    // Should succeed
    let result = discover_repositories(workspace.path(), DiscoveryMode::IndexFiles);
    assert!(
        result.is_ok(),
        "Discovery within limit should succeed: {result:?}"
    );
    assert_eq!(
        result.unwrap().len(),
        5,
        "Should find exactly 5 repositories"
    );
}

#[test]
#[serial]
fn test_repository_discovery_exceeds_limit() {
    use sqry_core::workspace::discovery::{DiscoveryMode, discover_repositories};

    // Create workspace with more repositories than the default limit (1000)
    // We'll create 1001 repositories to exceed the limit
    let workspace = TempDir::new().unwrap();

    // Create 1001 repositories (exceeds default limit of 1000)
    for i in 0..1001 {
        let repo_dir = workspace.path().join(format!("repo{i:04}"));
        std::fs::create_dir_all(&repo_dir).unwrap();
        std::fs::write(repo_dir.join(".sqry-index"), b"{}").unwrap();
    }

    // Should fail when trying to add the 1001st repository
    let result = discover_repositories(workspace.path(), DiscoveryMode::IndexFiles);
    assert!(
        result.is_err(),
        "Discovery exceeding default limit of 1000 should fail"
    );

    let error = result.err().unwrap().to_string();
    assert!(
        error.contains("Too many repositories"),
        "Error should mention too many repositories: {error}"
    );
    assert!(
        error.contains("SQRY_MAX_REPOSITORIES"),
        "Error should mention environment variable: {error}"
    );
}

// ============================================================================
// Gap #3: Watch Event Queue Capacity Tests
// ============================================================================

#[test]
#[serial]
fn test_watch_event_queue_config_function() {
    clear_env("SQRY_WATCH_EVENT_QUEUE");

    // Test default
    let default_capacity = watch_event_queue_capacity();
    assert_eq!(default_capacity, 10_000, "Default should be 10,000");

    // Test custom value
    set_env("SQRY_WATCH_EVENT_QUEUE", "50000");
    let custom_capacity = watch_event_queue_capacity();
    assert_eq!(custom_capacity, 50_000, "Custom value should be 50,000");

    // Test clamping (below minimum)
    set_env("SQRY_WATCH_EVENT_QUEUE", "50"); // Below 100 minimum
    let clamped_low = watch_event_queue_capacity();
    assert_eq!(clamped_low, 100, "Should clamp to 100 minimum");

    // Test clamping (above maximum)
    set_env("SQRY_WATCH_EVENT_QUEUE", "200000"); // Above 100,000 maximum
    let clamped_high = watch_event_queue_capacity();
    assert_eq!(clamped_high, 100_000, "Should clamp to 100,000 maximum");

    clear_env("SQRY_WATCH_EVENT_QUEUE");
}

#[test]
#[serial]
fn test_file_watcher_bounded_channel() {
    use sqry_core::session::watcher::FileWatcher;

    clear_env("SQRY_WATCH_EVENT_QUEUE");

    // Set custom capacity of 1000
    set_env("SQRY_WATCH_EVENT_QUEUE", "1000");

    // Create file watcher
    let result = FileWatcher::new();
    assert!(result.is_ok(), "File watcher creation should succeed");

    // Note: We can't directly test the channel capacity from the outside,
    // but we can verify the configuration function returns the correct value
    let capacity = watch_event_queue_capacity();
    assert_eq!(
        capacity, 1000,
        "Watch event queue should have capacity of 1000"
    );

    clear_env("SQRY_WATCH_EVENT_QUEUE");
}

// ============================================================================
// Gap #4: Query Length and Predicate Limits Tests
// ============================================================================

#[test]
#[serial]
fn test_query_length_config_function() {
    clear_env("SQRY_MAX_QUERY_LENGTH");

    // Test default
    let default_length = max_query_length();
    assert_eq!(default_length, 10 * 1024, "Default should be 10 KB");

    // Test custom value
    set_env("SQRY_MAX_QUERY_LENGTH", "51200"); // 50 KB
    let custom_length = max_query_length();
    assert_eq!(custom_length, 51_200, "Custom value should be 50 KB");

    // Test clamping (below minimum)
    set_env("SQRY_MAX_QUERY_LENGTH", "500"); // Below 1 KB minimum
    let clamped_low = max_query_length();
    assert_eq!(clamped_low, 1024, "Should clamp to 1 KB minimum");

    // Test clamping (above maximum)
    set_env("SQRY_MAX_QUERY_LENGTH", "200000"); // Above 100 KB maximum
    let clamped_high = max_query_length();
    assert_eq!(clamped_high, 102_400, "Should clamp to 100 KB maximum");

    clear_env("SQRY_MAX_QUERY_LENGTH");
}

#[test]
#[serial]
fn test_query_length_within_limit() {
    clear_env("SQRY_MAX_QUERY_LENGTH");

    // Create query within 10 KB limit
    let query_str = "kind:function AND name:test AND lang:rust AND path:src";
    let result = QueryParser::parse_query(query_str);

    assert!(
        result.is_ok(),
        "Query within limit should parse successfully: {result:?}"
    );
}

#[test]
#[serial]
fn test_query_length_exceeds_limit() {
    // Set custom limit of 1 KB for easier testing
    set_env("SQRY_MAX_QUERY_LENGTH", "1024");

    // Create 2 KB query (exceeds limit)
    let long_pattern = "a".repeat(2000); // 2 KB of 'a' characters
    let query_str = format!("name:{long_pattern}");

    let result = QueryParser::parse_query(&query_str);
    assert!(
        result.is_err(),
        "Query exceeding length limit should fail: {result:?}"
    );

    let error = result.unwrap_err().to_string();
    assert!(
        error.contains("Query too long"),
        "Error should mention query too long: {error}"
    );
    assert!(
        error.contains("SQRY_MAX_QUERY_LENGTH"),
        "Error should mention environment variable: {error}"
    );

    clear_env("SQRY_MAX_QUERY_LENGTH");
}

#[test]
#[serial]
fn test_predicate_count_config_function() {
    clear_env("SQRY_MAX_PREDICATES");

    // Test default
    let default_count = max_predicates();
    assert_eq!(default_count, 100, "Default should be 100");

    // Test custom value
    set_env("SQRY_MAX_PREDICATES", "500");
    let custom_count = max_predicates();
    assert_eq!(custom_count, 500, "Custom value should be 500");

    // Test clamping (below minimum)
    set_env("SQRY_MAX_PREDICATES", "5"); // Below 10 minimum
    let clamped_low = max_predicates();
    assert_eq!(clamped_low, 10, "Should clamp to 10 minimum");

    // Test clamping (above maximum)
    set_env("SQRY_MAX_PREDICATES", "2000"); // Above 1000 maximum
    let clamped_high = max_predicates();
    assert_eq!(clamped_high, 1000, "Should clamp to 1000 maximum");

    clear_env("SQRY_MAX_PREDICATES");
}

#[test]
#[serial]
fn test_predicate_count_within_limit() {
    clear_env("SQRY_MAX_PREDICATES");

    // Create query with 5 predicates (well within 100 limit)
    let query_str = "kind:function AND name:test AND lang:rust AND path:src AND parent:Module";
    let result = QueryParser::parse_query(query_str);

    assert!(
        result.is_ok(),
        "Query with 5 predicates should parse successfully: {result:?}"
    );
}

#[test]
#[serial]
fn test_predicate_count_exceeds_limit() {
    // Create query with 101 predicates (exceeds default limit of 100)
    let predicates: Vec<String> = (0..101).map(|i| format!("name:test{i}")).collect();
    let query_str = predicates.join(" AND ");

    let result = QueryParser::parse_query(&query_str);
    assert!(
        result.is_err(),
        "Query with 101 predicates should fail (default limit is 100)"
    );

    let error = result.err().unwrap().to_string();
    assert!(
        error.contains("Too many predicates"),
        "Error should mention too many predicates: {error}"
    );
    assert!(
        error.contains("SQRY_MAX_PREDICATES"),
        "Error should mention environment variable: {error}"
    );
}

// ============================================================================
// Integration Tests: Multiple Limits Combined
// ============================================================================

#[test]
#[serial]
fn test_dos_hardened_environment() {
    // Simulate high-security environment with strict limits
    set_env("SQRY_MAX_SOURCE_FILE_SIZE", "1048576"); // 1 MB
    set_env("SQRY_MAX_REPOSITORIES", "10");
    set_env("SQRY_MAX_QUERY_LENGTH", "1024"); // 1 KB
    set_env("SQRY_MAX_PREDICATES", "10");
    set_env("SQRY_WATCH_EVENT_QUEUE", "1000");

    // Verify all limits are enforced
    assert_eq!(max_source_file_size(), 1_048_576, "File size limit");
    assert_eq!(max_repositories(), 10, "Repository count limit");
    assert_eq!(max_query_length(), 1024, "Query length limit");
    assert_eq!(max_predicates(), 10, "Predicate count limit");
    assert_eq!(watch_event_queue_capacity(), 1000, "Event queue capacity");

    // Clean up
    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");
    clear_env("SQRY_MAX_REPOSITORIES");
    clear_env("SQRY_MAX_QUERY_LENGTH");
    clear_env("SQRY_MAX_PREDICATES");
    clear_env("SQRY_WATCH_EVENT_QUEUE");
}

#[test]
#[serial]
fn test_ci_cd_environment() {
    // Simulate CI/CD environment with relaxed limits
    set_env("SQRY_MAX_SOURCE_FILE_SIZE", "104857600"); // 100 MB
    set_env("SQRY_MAX_REPOSITORIES", "5000");
    set_env("SQRY_MAX_QUERY_LENGTH", "51200"); // 50 KB
    set_env("SQRY_MAX_PREDICATES", "500");
    set_env("SQRY_WATCH_EVENT_QUEUE", "50000");

    // Verify all limits are set correctly
    assert_eq!(max_source_file_size(), 104_857_600, "File size limit");
    assert_eq!(max_repositories(), 5000, "Repository count limit");
    assert_eq!(max_query_length(), 51_200, "Query length limit");
    assert_eq!(max_predicates(), 500, "Predicate count limit");
    assert_eq!(watch_event_queue_capacity(), 50_000, "Event queue capacity");

    // Clean up
    clear_env("SQRY_MAX_SOURCE_FILE_SIZE");
    clear_env("SQRY_MAX_REPOSITORIES");
    clear_env("SQRY_MAX_QUERY_LENGTH");
    clear_env("SQRY_MAX_PREDICATES");
    clear_env("SQRY_WATCH_EVENT_QUEUE");
}
