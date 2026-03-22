//! Integration tests for cache prune functionality.
//!
//! These tests validate the cache prune engine with realistic scenarios:
//! - Large cache directories (1000+ entries)
//! - Performance requirements (< 2s for 10k entries)
//! - Combined age and size policies
//! - Dry-run safety guarantees

use sqry_core::cache::{PruneOptions, PruneOutputMode};
use std::fs;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

use sqry_core::test_support::verbosity;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

/// Helper to create a synthetic cache entry
fn create_cache_entry(
    cache_dir: &std::path::Path,
    name: &str,
    size_bytes: u64,
    age_days: u64,
) -> std::path::PathBuf {
    // Create nested directory structure matching cache layout
    let user_dir = cache_dir.join("test_user");
    let lang_dir = user_dir.join("rust");
    let hash_dir = lang_dir.join("abc123def456");
    let path_dir = hash_dir.join("path1");

    fs::create_dir_all(&path_dir).unwrap();

    let file_path = path_dir.join(format!("{name}.bin"));
    // File size may exceed usize::MAX on 32-bit systems; clamp to max
    let size = size_bytes.try_into().unwrap_or(usize::MAX);
    let content = vec![0u8; size];
    fs::write(&file_path, content).unwrap();

    // Set modification time to simulate age
    let mtime = SystemTime::now() - Duration::from_secs(age_days * 24 * 3600);
    filetime::set_file_mtime(&file_path, filetime::FileTime::from_system_time(mtime)).unwrap();

    file_path
}

#[test]
fn test_large_directory_age_prune() {
    init_logging();
    log::info!("Testing cache prune with 1000 entries (performance benchmark)");
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create 1000 entries with varying ages
    let start = std::time::Instant::now();

    for i in 0..1000 {
        let age_days = if i < 300 {
            45 // Old entries
        } else if i < 700 {
            20 // Medium age
        } else {
            5 // Recent entries
        };

        create_cache_entry(cache_dir, &format!("file_{i}"), 1024, age_days);
    }

    let creation_time = start.elapsed();
    log::debug!("Created 1000 entries in {creation_time:?}");

    // Prune entries older than 30 days
    // Use PruneEngine directly to avoid user_hash layer from CacheManager
    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(30 * 24 * 3600))
        .with_output_mode(PruneOutputMode::Human);

    let prune_start = std::time::Instant::now();
    log::debug!("Starting prune operation (max age: 30 days)");
    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();
    let prune_time = prune_start.elapsed();

    log::debug!("Prune completed in {prune_time:?}");
    log::debug!("Entries considered: {}", report.entries_considered);
    log::debug!("Entries removed: {}", report.entries_removed);
    log::debug!("Entries remaining: {}", report.remaining_entries);

    // Assertions
    assert_eq!(report.entries_considered, 1000);
    assert_eq!(report.entries_removed, 300); // Entries > 30 days old
    assert_eq!(report.remaining_entries, 700);

    // Performance check: Should complete in < 2 seconds for 1000 entries
    // (Spec requires < 2s for 10k entries, so 1k should be much faster)
    log::info!(
        "✓ Pruned 1000 entries in {:?} - removed {}, kept {}",
        prune_time,
        report.entries_removed,
        report.remaining_entries
    );
    assert!(
        prune_time < Duration::from_secs(1),
        "Prune took {prune_time:?}, expected < 1s for 1000 entries"
    );
}

#[test]
fn test_mixed_age_and_size_prune() {
    init_logging();
    log::info!("Testing combined age + size policy (mixed eviction)");
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create 100 entries (1KB each = 100KB total)
    // 30 old entries (> 14 days)
    // 70 recent entries (< 14 days)
    for i in 0..100 {
        let age_days = if i < 30 { 20 } else { 5 };
        create_cache_entry(cache_dir, &format!("entry_{i}"), 1024, age_days);
    }

    // Policy: Remove entries > 14 days AND cap to 50KB
    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(14 * 24 * 3600))
        .with_max_size(50 * 1024) // 50KB
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();

    log::debug!(
        "Mixed policy results: removed {}, remaining {} ({} bytes)",
        report.entries_removed,
        report.remaining_entries,
        report.remaining_bytes
    );

    // Should remove:
    // - 30 old entries (age policy)
    // - Additional ~20 entries to get under 50KB (size policy)
    log::info!(
        "✓ Mixed policy: removed {} entries (age + size), kept {} bytes <= 50KB",
        report.entries_removed,
        report.remaining_bytes
    );
    assert!(
        report.entries_removed >= 30,
        "Should remove at least 30 old entries"
    );
    assert!(
        report.remaining_bytes <= 50 * 1024,
        "Should cap to 50KB, got {} bytes",
        report.remaining_bytes
    );
}

#[test]
fn test_dry_run_no_side_effects() {
    init_logging();
    log::info!("Testing dry-run mode has no side effects");
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create 50 old entries
    let mut file_paths = Vec::new();
    for i in 0..50 {
        let path = create_cache_entry(cache_dir, &format!("old_{i}"), 2048, 45);
        file_paths.push(path);
    }

    // Dry-run prune
    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(30 * 24 * 3600))
        .with_dry_run(true)
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();

    // Verify report shows deletions would happen
    log::debug!(
        "Dry-run report: {} entries would be removed",
        report.entries_removed
    );
    assert_eq!(report.entries_removed, 50);

    // Verify NO files were actually deleted
    let mut existing_count = 0;
    for path in &file_paths {
        assert!(
            path.exists(),
            "Dry-run should not delete files, but {path:?} is missing"
        );
        existing_count += 1;
    }
    log::info!(
        "✓ Dry-run: reported {} removals, verified {} files still exist",
        report.entries_removed,
        existing_count
    );
}

#[test]
fn test_handles_partial_failures() {
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create 10 normal entries
    for i in 0..10 {
        create_cache_entry(cache_dir, &format!("entry_{i}"), 1024, 45);
    }

    // Create one read-only directory to simulate permission issues
    let readonly_dir = cache_dir
        .join("test_user")
        .join("rust")
        .join("readonly_hash");
    fs::create_dir_all(&readonly_dir).unwrap();

    let readonly_file = readonly_dir.join("readonly.bin");
    fs::write(&readonly_file, vec![0u8; 1024]).unwrap();

    // Make file read-only on Unix systems
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&readonly_file).unwrap().permissions();
        perms.set_mode(0o444); // Read-only
        fs::set_permissions(&readonly_file, perms).unwrap();
    }

    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(30 * 24 * 3600))
        .with_output_mode(PruneOutputMode::Human);

    // Should succeed despite one file failing to delete
    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let result = engine.execute(cache_dir);
    assert!(result.is_ok(), "Prune should handle partial failures");

    // Clean up: restore permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&readonly_file).unwrap().permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&readonly_file, perms).unwrap();
    }
}

#[test]
fn test_custom_path_outside_default() {
    let tmp_cache_dir = TempDir::new().unwrap();
    let custom_cache = tmp_cache_dir.path().join("custom_cache");
    fs::create_dir_all(&custom_cache).unwrap();

    // Create entries in custom location
    for i in 0..20 {
        create_cache_entry(&custom_cache, &format!("custom_{i}"), 512, 40);
    }

    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(30 * 24 * 3600))
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(&custom_cache).unwrap();

    assert_eq!(report.entries_removed, 20);
}

#[test]
fn test_respects_lock_files() {
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create entry with lock file
    let data_path = create_cache_entry(cache_dir, "locked_entry", 1024, 45);
    let lock_path = data_path.with_extension("bin.lock");
    fs::write(&lock_path, b"12345").unwrap(); // Fake PID

    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(30 * 24 * 3600))
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();

    // Should remove both data file and lock file
    assert_eq!(report.entries_removed, 1);
    assert!(!data_path.exists(), "Data file should be deleted");
    assert!(!lock_path.exists(), "Lock file should be deleted");
}

#[test]
fn test_size_policy_removes_oldest_first() {
    init_logging();
    log::info!("Testing size policy removes oldest entries first");

    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create 10 entries with different ages (all 10KB each = 100KB total)
    // Oldest = file_9 (19 days), Newest = file_0 (10 days)
    for i in 0..10 {
        let age_days = 10 + i; // file_0 is 10 days old, file_9 is 19 days old
        create_cache_entry(cache_dir, &format!("file_{i}"), 10 * 1024, age_days);
    }
    log::debug!("Created 10 entries, each 10KB (100KB total)");

    // Cap to 50KB (should remove 5 oldest files)
    let options = PruneOptions::new()
        .with_max_size(50 * 1024)
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();

    log::debug!("Size policy results:");
    log::debug!("  Removed: {}", report.entries_removed);
    log::debug!("  Remaining: {}", report.remaining_entries);
    log::debug!("  Remaining bytes: {}", report.remaining_bytes);

    assert_eq!(report.entries_removed, 5);
    assert_eq!(report.remaining_entries, 5);
    assert!(report.remaining_bytes <= 50 * 1024);

    // Verify oldest files were removed (file_5-9 are 15-19 days old)
    for i in 5..10 {
        let path = cache_dir
            .join("test_user/rust/abc123def456/path1")
            .join(format!("file_{i}.bin"));
        assert!(!path.exists(), "Oldest file {i} should be deleted");
    }

    // Verify newest files remain (file_0-4 are 10-14 days old)
    for i in 0..5 {
        let path = cache_dir
            .join("test_user/rust/abc123def456/path1")
            .join(format!("file_{i}.bin"));
        assert!(path.exists(), "Newest file {i} should remain");
    }

    log::info!(
        "✓ Size policy removed oldest 5 entries first: {} bytes remaining <= 50KB",
        report.remaining_bytes
    );
}

#[test]
fn test_empty_cache_returns_zero_report() {
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(7 * 24 * 3600))
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();

    assert_eq!(report.entries_considered, 0);
    assert_eq!(report.entries_removed, 0);
    assert_eq!(report.remaining_entries, 0);
}

#[test]
fn test_cache_within_limits_no_removal() {
    let tmp_cache_dir = TempDir::new().unwrap();
    let cache_dir = tmp_cache_dir.path();

    // Create 5 recent entries (2KB each = 10KB total)
    for i in 0..5 {
        create_cache_entry(cache_dir, &format!("recent_{i}"), 2048, 3);
    }

    // All entries are recent (3 days) and cache is small
    let options = PruneOptions::new()
        .with_max_age(Duration::from_secs(7 * 24 * 3600)) // Keep 7 days
        .with_max_size(100 * 1024) // 100KB limit
        .with_output_mode(PruneOutputMode::Human);

    let engine = sqry_core::cache::PruneEngine::new(options).unwrap();
    let report = engine.execute(cache_dir).unwrap();

    assert_eq!(report.entries_considered, 5);
    assert_eq!(report.entries_removed, 0);
    assert_eq!(report.remaining_entries, 5);
}
