//! Integration tests for file I/O strategy selection (P1-17)
//!
//! Validates that mmap threshold correctly selects between buffered and
//! memory-mapped I/O strategies, and that both produce identical results.

use serial_test::serial;
use sqry_core::config::buffers::mmap_threshold;
use sqry_core::hash::hash_bytes;
use sqry_core::io::file_reader::{FileReader, ReaderPolicy};
use std::io::Write;
use tempfile::NamedTempFile;

/// Scenario 3 & 4: Mmap threshold correctly selects I/O strategy based on file size
#[test]
#[serial]
fn test_mmap_threshold_strategy_selection() {
    // Clean environment
    unsafe {
        std::env::remove_var("SQRY_MMAP_THRESHOLD");
    }

    let threshold = mmap_threshold();
    assert_eq!(
        threshold,
        10 * 1024 * 1024,
        "Default threshold should be 10MB"
    );

    // Scenario 3: Small file (< threshold) uses buffered I/O
    let mut small_file = NamedTempFile::new().expect("Failed to create temp file");
    let small_size = usize::try_from(threshold / 2).expect("Threshold should fit in usize"); // 5MB
    let small_data = vec![b'A'; small_size];
    small_file
        .write_all(&small_data)
        .expect("Failed to write small file");
    small_file.flush().expect("Failed to flush small file");

    let reader_small =
        FileReader::open_with_policy(small_file.path(), ReaderPolicy::Auto { threshold })
            .expect("Failed to open small file");

    // Verify buffered strategy used
    match reader_small {
        FileReader::Buffered { .. } => {
            // Expected: small file uses buffered I/O
        }
        FileReader::Mmap { .. } => {
            panic!("Small file ({small_size} bytes) should use buffered I/O, not mmap");
        }
    }

    // Scenario 4: Large file (> threshold) uses memory-mapped I/O
    let mut large_file = NamedTempFile::new().expect("Failed to create temp file");
    let large_size = usize::try_from(threshold * 2).expect("Threshold should fit in usize"); // 20MB
    let large_data = vec![b'B'; large_size];
    large_file
        .write_all(&large_data)
        .expect("Failed to write large file");
    large_file.flush().expect("Failed to flush large file");

    let reader_large =
        FileReader::open_with_policy(large_file.path(), ReaderPolicy::Auto { threshold })
            .expect("Failed to open large file");

    // Verify mmap strategy used
    match reader_large {
        FileReader::Mmap { .. } => {
            // Expected: large file uses mmap
        }
        FileReader::Buffered { .. } => {
            panic!("Large file ({large_size} bytes) should use mmap, not buffered I/O");
        }
    }

    unsafe {
        std::env::remove_var("SQRY_MMAP_THRESHOLD");
    }
}

/// Scenario 5: Correctness guarantee - same hash regardless of I/O strategy
#[test]
#[serial]
fn test_mmap_correctness_guarantee() {
    // Create 20MB test file
    let test_size: usize = 20 * 1024 * 1024;
    let test_data: Vec<u8> = (0..test_size)
        .map(|i| u8::try_from(i % 256).expect("i % 256 fits in u8"))
        .collect();

    let mut file = NamedTempFile::new().expect("Failed to create temp file");
    file.write_all(&test_data)
        .expect("Failed to write test file");
    file.flush().expect("Failed to flush test file");

    // Read with buffered strategy
    let reader_buffered = FileReader::open_with_policy(file.path(), ReaderPolicy::Buffered)
        .expect("Failed to open file with buffered policy");
    let hash_buffered = hash_bytes(reader_buffered.as_slice());

    // Read with mmap strategy
    let reader_mmap = FileReader::open_with_policy(file.path(), ReaderPolicy::Mmap)
        .expect("Failed to open file with mmap policy");
    let hash_mmap = hash_bytes(reader_mmap.as_slice());

    // Correctness guarantee: hashes must be identical
    assert_eq!(
        hash_buffered, hash_mmap,
        "Hash must be identical regardless of I/O strategy (buffered vs mmap)"
    );

    // Verify data integrity
    assert_eq!(
        reader_buffered.as_slice(),
        &test_data[..],
        "Buffered reader should return exact data"
    );
    assert_eq!(
        reader_mmap.as_slice(),
        &test_data[..],
        "Mmap reader should return exact data"
    );
}

/// Scenario 3-5 combined: Test custom threshold with correctness validation
#[test]
#[serial]
fn test_custom_mmap_threshold_with_correctness() {
    // Set custom threshold: 5MB
    let custom_threshold = 5 * 1024 * 1024;
    unsafe {
        std::env::set_var("SQRY_MMAP_THRESHOLD", custom_threshold.to_string());
    }

    assert_eq!(mmap_threshold(), custom_threshold);

    // Create 8MB test file (above custom 5MB threshold)
    let test_size: usize = 8 * 1024 * 1024;
    let test_data: Vec<u8> = (0..test_size)
        .map(|i| u8::try_from(i % 256).expect("i % 256 fits in u8"))
        .collect();

    let mut file = NamedTempFile::new().expect("Failed to create temp file");
    file.write_all(&test_data)
        .expect("Failed to write test file");
    file.flush().expect("Failed to flush test file");

    // With custom threshold, 8MB file should use mmap
    let reader_auto = FileReader::open_with_policy(
        file.path(),
        ReaderPolicy::Auto {
            threshold: custom_threshold,
        },
    )
    .expect("Failed to open file with auto policy");

    match reader_auto {
        FileReader::Mmap { .. } => {
            // Expected: 8MB > 5MB threshold, should use mmap
        }
        FileReader::Buffered { .. } => {
            panic!("8MB file should use mmap with 5MB threshold");
        }
    }

    // Verify correctness with explicit strategies
    let reader_buffered = FileReader::open_with_policy(file.path(), ReaderPolicy::Buffered)
        .expect("Failed to open file with buffered policy");
    let reader_mmap = FileReader::open_with_policy(file.path(), ReaderPolicy::Mmap)
        .expect("Failed to open file with mmap policy");

    assert_eq!(
        hash_bytes(reader_buffered.as_slice()),
        hash_bytes(reader_mmap.as_slice()),
        "Custom threshold should not affect correctness"
    );

    unsafe {
        std::env::remove_var("SQRY_MMAP_THRESHOLD");
    }
}
