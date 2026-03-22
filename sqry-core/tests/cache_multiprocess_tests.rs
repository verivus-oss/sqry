//! Multi-process integration tests for cache persistence layer.
//!
//! These tests verify that the cache module correctly handles concurrent access
//! from multiple separate processes, ensuring:
//! - Lock files prevent data corruption
//! - Stale locks are cleaned up after process crashes
//! - Concurrent reads and writes maintain data integrity
//! - Lock timeouts and retries work correctly
//!
//! # Test Infrastructure
//!
//! Each test spawns actual child processes that interact with the cache, then
//! verifies the results through IPC (inter-process communication via temp files).

use sqry_core::cache::CacheKey;
use sqry_core::cache::{CacheConfig, CacheManager, GraphNodeSummary, PersistManager};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::hash::{Blake3Hash, hash_bytes};
use sqry_core::test_support::verbosity;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Once};
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;
use tempfile::TempDir;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

// ============================================================================
// Test Infrastructure
// ============================================================================

/// Helper to create test content hash
fn make_content_hash(content: &str) -> Blake3Hash {
    hash_bytes(content.as_bytes())
}

/// Helper to create test symbol summaries
fn make_test_summaries(names: &[&str], file: &str) -> Vec<GraphNodeSummary> {
    names
        .iter()
        .map(|name| {
            GraphNodeSummary::new(
                Arc::from(*name),
                NodeKind::Function,
                Arc::from(Path::new(file)),
                1,
                0,
                10,
                0,
            )
        })
        .collect()
}

struct ManualLockGuard {
    path: PathBuf,
    file: fs::File,
}

impl Drop for ManualLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Result file for child process communication
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ChildResult {
    success: bool,
    message: String,
    data: Option<Vec<String>>, // Symbol names read from cache
}

impl ChildResult {
    fn success(message: String, data: Option<Vec<String>>) -> Self {
        Self {
            success: true,
            message,
            data,
        }
    }

    fn failure(message: String) -> Self {
        Self {
            success: false,
            message,
            data: None,
        }
    }

    fn write_to_file(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)
    }

    fn read_from_file(path: &Path) -> std::io::Result<Self> {
        let json = fs::read_to_string(path)?;
        serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// Spawn a child process that runs a cache operation
///
/// The child process will:
/// 1. Parse command-line arguments
/// 2. Execute the requested cache operation
/// 3. Write results to a JSON file for parent to read
///
/// # Arguments
///
/// - `test_name`: Unique identifier for this test
/// - `cache_dir`: Path to the shared cache directory
/// - `result_file`: Path where child writes its results
/// - `operation`: The cache operation to perform
/// - `args`: Additional arguments (key, content, delay, etc.)
fn spawn_cache_child_process(
    _test_name: &str,
    cache_dir: &Path,
    result_file: &Path,
    operation: &str,
    args: &[&str],
) -> std::process::Child {
    // Build the command to run this test binary in child mode
    let exe = std::env::current_exe().expect("Failed to get test executable path");

    let mut cmd = Command::new(exe);
    let child_test_name = format!("cache_child_{operation}");
    cmd.arg("--nocapture")
        .arg("--ignored") // Child processes run as ignored tests
        .arg("--exact")
        .arg(&child_test_name)
        .env("CACHE_CHILD_MODE", "1")
        .env("CACHE_DIR", cache_dir)
        .env("RESULT_FILE", result_file);

    // Add operation-specific args
    for (i, arg) in args.iter().enumerate() {
        cmd.env(format!("ARG_{i}"), arg);
    }

    cmd.stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn child process")
}

/// Check if we're running as a child process
fn is_child_process() -> bool {
    std::env::var("CACHE_CHILD_MODE").is_ok()
}

/// Get child process environment variables
fn get_child_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("Missing env var: {key}"))
}

/// Wait for a child process to finish, surfacing stderr if it fails.
fn wait_for_child(child: &mut std::process::Child, label: &str) {
    let status = child
        .wait()
        .unwrap_or_else(|e| panic!("Failed to wait for {label}: {e}"));

    if !status.success() {
        let mut stderr_output = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = stderr.read_to_string(&mut stderr_output);
        }
        panic!("{label} exited with status {status:?}. Stderr:\n{stderr_output}");
    }
}

// ============================================================================
// Child Process Entry Points
// ============================================================================

/// Child process: Write to cache
#[test]
#[ignore = "Only run when spawned as child process"]
fn cache_child_write_entry() {
    if !is_child_process() {
        return;
    }

    let cache_dir = PathBuf::from(get_child_env("CACHE_DIR"));
    let result_file = PathBuf::from(get_child_env("RESULT_FILE"));
    let file_path = get_child_env("ARG_0");
    let content = get_child_env("ARG_1");
    let symbol_names = get_child_env("ARG_2");

    let result: Result<ChildResult, String> = {
        // Create cache config pointing to shared directory
        let config = CacheConfig::new()
            .with_cache_root(cache_dir)
            .with_persistence(true);

        let cache = CacheManager::new(config);

        // Create content hash and summaries
        let hash = make_content_hash(&content);
        let names: Vec<&str> = symbol_names.split(',').collect();
        let summaries = make_test_summaries(&names, &file_path);

        // Write to cache
        cache.insert(&file_path, "rust", hash, summaries);

        Ok(ChildResult::success(
            format!("Wrote {} symbols to cache", names.len()),
            Some(names.iter().map(std::string::ToString::to_string).collect()),
        ))
    };

    let final_result = result.unwrap_or_else(ChildResult::failure);
    final_result
        .write_to_file(&result_file)
        .expect("Failed to write result file");
}

/// Child process: Read from cache
#[test]
#[ignore = "Only run when spawned as child process"]
fn cache_child_read_entry() {
    if !is_child_process() {
        return;
    }

    let cache_dir = PathBuf::from(get_child_env("CACHE_DIR"));
    let result_file = PathBuf::from(get_child_env("RESULT_FILE"));
    let file_path = get_child_env("ARG_0");
    let content = get_child_env("ARG_1");

    let result: Result<ChildResult, String> = {
        let config = CacheConfig::new()
            .with_cache_root(cache_dir)
            .with_persistence(true);

        let cache = CacheManager::new(config);
        let hash = make_content_hash(&content);

        // Try to read from cache
        match cache.get(&file_path, "rust", hash) {
            Some(summaries) => {
                let names: Vec<String> = summaries.iter().map(|s| s.name.to_string()).collect();
                Ok(ChildResult::success(
                    format!("Read {} symbols from cache", names.len()),
                    Some(names),
                ))
            }
            None => Ok(ChildResult::success("Cache miss".to_string(), None)),
        }
    };

    let final_result = result.unwrap_or_else(ChildResult::failure);
    final_result
        .write_to_file(&result_file)
        .expect("Failed to write result file");
}

/// Child process: Hold lock for a duration
#[test]
#[ignore = "Only run when spawned as child process"]
fn cache_child_hold_lock() -> Result<(), String> {
    if !is_child_process() {
        return Ok(());
    }

    let cache_dir = PathBuf::from(get_child_env("CACHE_DIR"));
    let result_file = PathBuf::from(get_child_env("RESULT_FILE"));
    let file_path = get_child_env("ARG_0");
    let content = get_child_env("ARG_1");
    let hold_duration_ms: u64 = get_child_env("ARG_2").parse().expect("Invalid duration");

    let result: Result<ChildResult, String> = {
        let config = CacheConfig::new()
            .with_cache_root(cache_dir.clone())
            .with_persistence(true);

        let cache = CacheManager::new(config);
        let hash = make_content_hash(&content);
        let key = CacheKey::from_raw_path(PathBuf::from(&file_path), "rust", hash);

        // Create lock file manually to simulate long-running write
        let persist = PersistManager::new(cache_dir.clone())
            .map_err(|e| format!("Failed to initialize persistence: {e}"))?;

        // H1 FIX: Use production lock path construction (entry_path + set_extension)
        let entry_path = persist
            .user_cache_dir()
            .join(format!("{}.bin", key.storage_key()));
        let mut lock_path = entry_path.clone();
        lock_path.set_extension("bin.lock");

        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Failed to create lock directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }

        let lock_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|e| format!("Failed to create lock file {}: {}", lock_path.display(), e))?;

        let mut lock_guard = ManualLockGuard {
            path: lock_path.clone(),
            file: lock_file,
        };

        // Write PID to lock file
        writeln!(&mut lock_guard.file, "{}", std::process::id())
            .map_err(|e| format!("Failed to write lock PID: {e}"))?;
        lock_guard
            .file
            .sync_all()
            .map_err(|e| format!("Failed to sync lock file: {e}"))?;

        // Hold the lock by sleeping
        thread::sleep(Duration::from_millis(hold_duration_ms));

        // Lock released automatically via Drop
        drop(lock_guard); // Explicit drop to ensure lock is released before cache write

        // After releasing the lock, write to cache so callers observe normal behavior
        let summaries = make_test_summaries(&["held_fn"], &file_path);
        cache.insert(&file_path, "rust", hash, summaries);

        // Ensure cache is dropped and persisted before process exits
        drop(cache);
        thread::sleep(Duration::from_millis(50));

        Ok(ChildResult::success(
            format!("Held lock for {hold_duration_ms}ms"),
            None,
        ))
    };

    let final_result = result.unwrap_or_else(ChildResult::failure);
    final_result
        .write_to_file(&result_file)
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================================
// Multi-Process Integration Tests
// ============================================================================

#[test]
fn test_multiprocess_concurrent_writes() {
    init_logging();
    log::info!("Testing multi-process concurrent writes (lock contention)");

    // Skip if running as child process
    if is_child_process() {
        return;
    }

    let tmp_cache_dir = TempDir::new().expect("Failed to create temp dir");
    let cache_dir = tmp_cache_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("Failed to create cache dir");

    let file_path = "/test/file.rs";
    let content = "fn test() {}";

    // Spawn two child processes that both try to write to the same cache key
    let result_file_1 = tmp_cache_dir.path().join("result1.json");
    let result_file_2 = tmp_cache_dir.path().join("result2.json");

    log::debug!("Spawning child process 1 to write fn1,fn2,fn3");
    let mut child1 = spawn_cache_child_process(
        "concurrent_writes",
        &cache_dir,
        &result_file_1,
        "write_entry",
        &[file_path, content, "fn1,fn2,fn3"],
    );

    log::debug!("Spawning child process 2 to write fn4,fn5,fn6");
    let mut child2 = spawn_cache_child_process(
        "concurrent_writes",
        &cache_dir,
        &result_file_2,
        "write_entry",
        &[file_path, content, "fn4,fn5,fn6"],
    );

    // Wait for both children to complete
    log::debug!("Waiting for both child processes to complete");
    wait_for_child(&mut child1, "child 1 (concurrent_writes write_entry)");
    wait_for_child(&mut child2, "child 2 (concurrent_writes write_entry)");

    // Read results
    let result1 = ChildResult::read_from_file(&result_file_1).expect("Failed to read result 1");
    let result2 = ChildResult::read_from_file(&result_file_2).expect("Failed to read result 2");

    assert!(
        result1.success,
        "Child 1 operation failed: {}",
        result1.message
    );
    assert!(
        result2.success,
        "Child 2 operation failed: {}",
        result2.message
    );

    // Verify final cache state in parent process
    let config = CacheConfig::new()
        .with_cache_root(cache_dir.clone())
        .with_persistence(true);

    let cache = CacheManager::new(config);
    let hash = make_content_hash(content);

    let cached_summaries = cache
        .get(file_path, "rust", hash)
        .expect("Cache should have entry after writes");

    // Should have symbols from ONE of the two writes (not corrupted mix)
    assert_eq!(
        cached_summaries.len(),
        3,
        "Cache should have exactly 3 symbols from one complete write"
    );

    // Verify symbols are either all from child1 OR all from child2
    let names: Vec<String> = cached_summaries
        .iter()
        .map(|s| s.name.to_string())
        .collect();

    let is_child1_set = names
        .iter()
        .all(|n| n.starts_with("fn") && ["fn1", "fn2", "fn3"].contains(&n.as_str()));
    let is_child2_set = names
        .iter()
        .all(|n| n.starts_with("fn") && ["fn4", "fn5", "fn6"].contains(&n.as_str()));

    assert!(
        is_child1_set || is_child2_set,
        "Cache should contain complete set from one child, not a corrupted mix. Got: {names:?}"
    );

    log::info!("✓ Multi-process concurrent writes: No corruption detected. Final state: {names:?}");
}

#[test]
fn test_multiprocess_read_write_consistency() {
    init_logging();
    log::info!("Testing multi-process read-write consistency");

    if is_child_process() {
        return;
    }

    let tmp_cache_dir = TempDir::new().expect("Failed to create temp dir");
    let cache_dir = tmp_cache_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("Failed to create cache dir");

    let file_path = "/test/file.rs";
    let content = "fn test() {}";

    // Parent writes initial data
    let config = CacheConfig::new()
        .with_cache_root(cache_dir.clone())
        .with_persistence(true);

    let cache = CacheManager::new(config.clone());
    let hash = make_content_hash(content);
    let initial_summaries = make_test_summaries(&["initial_fn"], file_path);
    cache.insert(file_path, "rust", hash, initial_summaries);

    // Spawn child process to read
    let result_file = tmp_cache_dir.path().join("result.json");
    let mut child = spawn_cache_child_process(
        "read_write",
        &cache_dir,
        &result_file,
        "read_entry",
        &[file_path, content],
    );

    wait_for_child(&mut child, "child (read_write read_entry)");

    let result = ChildResult::read_from_file(&result_file).expect("Failed to read result");
    assert!(result.success, "Child read failed: {}", result.message);

    // Verify child read the correct data
    let read_names = result.data.expect("Child should have read data");
    assert_eq!(read_names, vec!["initial_fn"], "Child read incorrect data");

    log::info!("✓ Multi-process read-write consistency verified: child read {read_names:?}");
}

#[test]
#[cfg(target_os = "linux")] // This test requires process existence checking
fn test_multiprocess_stale_lock_cleanup() {
    init_logging();
    log::info!("Testing stale lock cleanup after process crash");

    if is_child_process() {
        return;
    }

    let tmp_cache_dir = TempDir::new().expect("Failed to create temp dir");
    let cache_dir = tmp_cache_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("Failed to create cache dir");

    let file_path = "/test/file.rs";
    let content = "fn test() {}";

    // Start a child that will hold a lock
    let result_file_1 = tmp_cache_dir.path().join("result1.json");
    let mut child1 = spawn_cache_child_process(
        "stale_lock",
        &cache_dir,
        &result_file_1,
        "hold_lock",
        &[file_path, content, "100"], // Hold for 100ms
    );

    // M2 FIX: Wait until lock is acquired, then verify it exists before cleanup
    let hash = make_content_hash(content);
    let key = CacheKey::from_raw_path(PathBuf::from(file_path), "rust", hash);
    let persist_check =
        PersistManager::new(cache_dir.clone()).expect("Failed to create persist manager");
    let entry_path = persist_check
        .user_cache_dir()
        .join(format!("{}.bin", key.storage_key()));
    let mut lock_path = entry_path.clone();
    lock_path.set_extension("bin.lock");

    // Wait for lock to be acquired (2 seconds max - allows headroom for coverage instrumentation)
    let mut lock_acquired = false;
    for _ in 0..200 {
        if lock_path.exists() {
            lock_acquired = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(lock_acquired, "Child 1 never acquired lock");

    // Kill the child process (simulate crash)
    log::debug!("Simulating process crash by killing child 1");
    child1.kill().expect("Failed to kill child process");

    // Wait to ensure process is dead
    let _ = child1.wait();
    thread::sleep(Duration::from_millis(100));

    // M2 FIX: Verify lock file still exists (proving it's now stale)
    assert!(
        lock_path.exists(),
        "Lock file should still exist after process crash (before cleanup): {lock_path:?}"
    );

    // Now try to acquire the same lock from a new process
    // This should succeed after detecting and cleaning up the stale lock
    let result_file_2 = tmp_cache_dir.path().join("result2.json");
    let mut child2 = spawn_cache_child_process(
        "stale_lock",
        &cache_dir,
        &result_file_2,
        "write_entry",
        &[file_path, content, "cleanup_fn"],
    );

    wait_for_child(&mut child2, "child 2 (stale_lock write_entry)");

    let result2 = ChildResult::read_from_file(&result_file_2).expect("Failed to read result 2");
    assert!(
        result2.success,
        "Child 2 should successfully write after cleaning stale lock: {}",
        result2.message
    );

    // M2 FIX: Verify lock was removed after successful acquisition and release
    assert!(
        !lock_path.exists(),
        "Stale lock should be cleaned up after child 2 completes: {lock_path:?}"
    );

    log::info!("✓ Stale lock cleanup verified: lock removed after process crash and recovery");
}

#[test]
#[cfg(target_os = "linux")]
fn test_multiprocess_lock_retry_succeeds() {
    init_logging();
    log::info!("Testing multi-process lock retry mechanism");

    if is_child_process() {
        return;
    }

    let tmp_cache_dir = TempDir::new().expect("Failed to create temp dir");
    let cache_dir = tmp_cache_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("Failed to create cache dir");

    let file_path = "/test/file.rs";
    let content = "fn test() {}";

    // Child 1: hold the lock for a while to force retries
    let result_file_lock = tmp_cache_dir.path().join("lock_holder.json");
    let mut lock_holder = spawn_cache_child_process(
        "lock_retry",
        &cache_dir,
        &result_file_lock,
        "hold_lock",
        &[file_path, content, "300"],
    );

    // M1 FIX: Poll until lock file actually exists (guarantees contention)
    let hash = make_content_hash(content);
    let key = CacheKey::from_raw_path(PathBuf::from(file_path), "rust", hash);
    let persist_check =
        PersistManager::new(cache_dir.clone()).expect("Failed to create persist manager");
    let entry_path = persist_check
        .user_cache_dir()
        .join(format!("{}.bin", key.storage_key()));
    let mut expected_lock_path = entry_path.clone();
    expected_lock_path.set_extension("bin.lock");

    let mut lock_ready = false;
    for _ in 0..200 {
        if expected_lock_path.exists() {
            lock_ready = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        lock_ready,
        "Lock holder never acquired lock at {expected_lock_path:?}"
    );

    // Child 2: attempt to write while lock is held; should retry until lock is released
    let result_file_writer = tmp_cache_dir.path().join("writer.json");
    let start = Instant::now();
    let mut writer = spawn_cache_child_process(
        "lock_retry",
        &cache_dir,
        &result_file_writer,
        "write_entry",
        &[file_path, content, "retry_fn"],
    );

    wait_for_child(&mut writer, "child 2 (lock_retry write_entry)");
    let elapsed = start.elapsed();

    // M1 FIX: Relax timing to allow 50ms variance for CI systems
    // M3 FIX: This timing check validates retry loop executed
    // Retry delay is 100ms, lock held for 300ms, so we should wait 200-300ms
    // This proves at least 2 retry attempts occurred (2 × 100ms = 200ms minimum)
    assert!(
        elapsed >= Duration::from_millis(200),
        "Writer should block until lock released, proving retry loop executed (waited {elapsed:?})"
    );

    let writer_result =
        ChildResult::read_from_file(&result_file_writer).expect("Failed to read writer result");
    assert!(
        writer_result.success,
        "Writer should succeed after retries: {}",
        writer_result.message
    );
    // Lock holder should also complete successfully
    wait_for_child(&mut lock_holder, "child 1 (lock_retry hold_lock)");
    let lock_result =
        ChildResult::read_from_file(&result_file_lock).expect("Failed to read lock holder result");
    assert!(
        lock_result.success,
        "Lock holder should succeed: {}",
        lock_result.message
    );

    // Parent verifies final cache state has data from one of the writers
    // Note: Since both lock holder and writer complete successfully, either may
    // be the final state depending on timing. The important thing is that both
    // processes successfully acquired the lock and wrote data without corruption.
    let config = CacheConfig::new()
        .with_cache_root(cache_dir.clone())
        .with_persistence(true);
    let cache = CacheManager::new(config);
    let hash = make_content_hash(content);

    let cached = cache
        .get(file_path, "rust", hash)
        .expect("Cache should have entry after both processes complete");
    let names: Vec<String> = cached.iter().map(|s| s.name.to_string()).collect();

    // Accept either "held_fn" (lock holder) or "retry_fn" (writer) as final state
    assert!(
        names == vec!["held_fn"] || names == vec!["retry_fn"],
        "Cache should contain data from one of the writers; got {names:?}"
    );

    log::info!(
        "✓ Multi-process lock retry succeeded after {elapsed:?} wait: final state {names:?}"
    );
}

#[test]
fn test_multiprocess_cache_persistence_across_restarts() {
    init_logging();
    log::info!("Testing cache persistence across process restarts");

    if is_child_process() {
        return;
    }

    let tmp_cache_dir = TempDir::new().expect("Failed to create temp dir");
    let cache_dir = tmp_cache_dir.path().join("cache");
    fs::create_dir_all(&cache_dir).expect("Failed to create cache dir");

    let file_path = "/test/file.rs";
    let content = "fn test() {}";

    // Process 1: Write to cache
    let result_file_1 = tmp_cache_dir.path().join("result1.json");
    let mut child1 = spawn_cache_child_process(
        "persistence",
        &cache_dir,
        &result_file_1,
        "write_entry",
        &[file_path, content, "persisted_fn"],
    );

    wait_for_child(&mut child1, "child 1 (persistence write_entry)");

    // Wait to ensure write is flushed to disk
    thread::sleep(Duration::from_millis(100));

    // Process 2: Read from cache (different process, should load from disk)
    let result_file_2 = tmp_cache_dir.path().join("result2.json");
    let mut child2 = spawn_cache_child_process(
        "persistence",
        &cache_dir,
        &result_file_2,
        "read_entry",
        &[file_path, content],
    );

    wait_for_child(&mut child2, "child 2 (persistence read_entry)");

    let result2 = ChildResult::read_from_file(&result_file_2).expect("Failed to read result 2");
    assert!(
        result2.success,
        "Child 2 read operation failed: {}",
        result2.message
    );

    let read_names = result2
        .data
        .expect("Child 2 should have read data from disk");
    assert_eq!(
        read_names,
        vec!["persisted_fn"],
        "Child 2 should read data persisted by child 1"
    );

    log::info!(
        "✓ Cache persistence across process restarts verified: child 2 read {read_names:?} from disk"
    );
}
