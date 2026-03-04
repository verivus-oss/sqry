//! Buffer size configuration for sqry I/O operations.
//!
//! This module is the single source of truth for all buffer sizes.
//! DO NOT use magic numbers elsewhere - import these constants.
//!
//! # Environment Variable Overrides
//!
//! All buffer sizes can be overridden via environment variables:
//! - `SQRY_READ_BUFFER`: Read buffer size in bytes
//! - `SQRY_WRITE_BUFFER`: Write buffer size in bytes
//! - `SQRY_PARSE_BUFFER`: Parse buffer size in bytes
//! - `SQRY_INDEX_BUFFER`: Index buffer size in bytes
//! - `SQRY_MMAP_THRESHOLD`: Memory-map threshold in bytes (P1-17)
//!
//! # Examples
//!
//! ```
//! use sqry_core::config::buffers::DEFAULT_READ_BUFFER;
//!
//! // Example: preallocate a byte buffer for reading
//! let buf: Vec<u8> = Vec::with_capacity(DEFAULT_READ_BUFFER);
//! assert!(buf.capacity() >= DEFAULT_READ_BUFFER);
//! ```

/// Read buffer size for file I/O (8 KB)
///
/// Optimal for filesystem block sizes (4-16 KB typical).
pub const DEFAULT_READ_BUFFER: usize = 8192;

/// Write buffer size for file I/O (8 KB)
///
/// Matches read buffer for symmetric I/O performance.
pub const DEFAULT_WRITE_BUFFER: usize = 8192;

/// Parse buffer size for tree-sitter parsing (64 KB)
///
/// Reduces overhead for large file parsing operations.
pub const DEFAULT_PARSE_BUFFER: usize = 65536;

/// Index buffer size for serialization (1 MB)
///
/// Minimizes syscalls for index I/O operations.
pub const DEFAULT_INDEX_BUFFER: usize = 1_048_576;

// Safety bounds for buffer sizes (P1-14: Security hardening)
const MIN_READ_BUFFER: usize = 1024; // 1 KB minimum
const MAX_READ_BUFFER: usize = 1_048_576; // 1 MB maximum
const MIN_WRITE_BUFFER: usize = 1024; // 1 KB minimum
const MAX_WRITE_BUFFER: usize = 1_048_576; // 1 MB maximum
const MIN_PARSE_BUFFER: usize = 4096; // 4 KB minimum
const MAX_PARSE_BUFFER: usize = 10_485_760; // 10 MB maximum
const MIN_INDEX_BUFFER: usize = 65536; // 64 KB minimum
const MAX_INDEX_BUFFER: usize = 104_857_600; // 100 MB maximum

/// Get read buffer size, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_READ_BUFFER` environment variable.
/// If not set or invalid, returns [`DEFAULT_READ_BUFFER`].
/// Values are clamped between 1 KB and 1 MB for safety (P1-14).
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::read_buffer_size;
///
/// let size = read_buffer_size();
/// assert!(size >= 1024); // At least 1 KB
/// assert!(size <= 1048576); // At most 1 MB
/// ```
#[must_use]
pub fn read_buffer_size() -> usize {
    let size = std::env::var("SQRY_READ_BUFFER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_READ_BUFFER);
    size.clamp(MIN_READ_BUFFER, MAX_READ_BUFFER)
}

/// Get write buffer size, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_WRITE_BUFFER` environment variable.
/// If not set or invalid, returns [`DEFAULT_WRITE_BUFFER`].
/// Values are clamped between 1 KB and 1 MB for safety (P1-14).
#[must_use]
pub fn write_buffer_size() -> usize {
    let size = std::env::var("SQRY_WRITE_BUFFER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WRITE_BUFFER);
    size.clamp(MIN_WRITE_BUFFER, MAX_WRITE_BUFFER)
}

/// Get parse buffer size, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_PARSE_BUFFER` environment variable.
/// If not set or invalid, returns [`DEFAULT_PARSE_BUFFER`].
/// Values are clamped between 4 KB and 10 MB for safety (P1-14).
#[must_use]
pub fn parse_buffer_size() -> usize {
    let size = std::env::var("SQRY_PARSE_BUFFER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PARSE_BUFFER);
    size.clamp(MIN_PARSE_BUFFER, MAX_PARSE_BUFFER)
}

/// Get index buffer size, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_INDEX_BUFFER` environment variable.
/// If not set or invalid, returns [`DEFAULT_INDEX_BUFFER`].
/// Values are clamped between 64 KB and 100 MB for safety (P1-14).
#[must_use]
pub fn index_buffer_size() -> usize {
    let size = std::env::var("SQRY_INDEX_BUFFER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_INDEX_BUFFER);
    size.clamp(MIN_INDEX_BUFFER, MAX_INDEX_BUFFER)
}

/// Mmap threshold for file I/O (10 MB)
///
/// Files larger than this threshold use memory-mapped I/O (mmap).
/// Files smaller use buffered reading (`Vec<u8>`).
pub const DEFAULT_MMAP_THRESHOLD: u64 = 10 * 1024 * 1024;

// Safety bounds for mmap threshold (P1-17: Security hardening)
const MIN_MMAP_THRESHOLD: u64 = 1024 * 1024; // 1 MB minimum
const MAX_MMAP_THRESHOLD: u64 = 1024 * 1024 * 1024; // 1 GB maximum

/// Maximum source file size for indexing (50 MB)
///
/// Files larger than this are rejected to prevent `DoS` attacks via
/// multi-gigabyte source files in malicious repositories (RR-10 Gap #1).
pub const DEFAULT_MAX_SOURCE_FILE_SIZE: u64 = 50 * 1024 * 1024;

// Safety bounds for max file size (RR-10: DoS prevention)
const MIN_MAX_SOURCE_FILE_SIZE: u64 = 1024 * 1024; // 1 MB minimum
const MAX_MAX_SOURCE_FILE_SIZE: u64 = 500 * 1024 * 1024; // 500 MB maximum

/// Maximum number of repositories per workspace (1,000)
///
/// Limits the number of repositories discovered during workspace scanning
/// to prevent `DoS` attacks via workspaces with thousands of .sqry-index files (RR-10 Gap #2).
pub const DEFAULT_MAX_REPOSITORIES: usize = 1_000;

// Safety bounds for repository count (RR-10: DoS prevention)
const MIN_MAX_REPOSITORIES: usize = 10; // 10 minimum (reasonable for development)
const MAX_MAX_REPOSITORIES: usize = 10_000; // 10k maximum (large monorepo + microservices)

/// Get mmap threshold, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_MMAP_THRESHOLD` environment variable.
/// If not set or invalid, returns [`DEFAULT_MMAP_THRESHOLD`].
/// Values are clamped between 1 MB and 1 GB for safety (P1-17).
///
/// # Platform Considerations
///
/// - **32-bit systems**: Lower values (e.g., 50MB) recommended
/// - **64-bit systems**: Default 10MB is safe
/// - **Containers**: Lower values reduce memory pressure
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::mmap_threshold;
///
/// let threshold = mmap_threshold();
/// assert!(threshold >= 1048576); // At least 1 MB
/// assert!(threshold <= 1073741824); // At most 1 GB
/// ```
#[must_use]
pub fn mmap_threshold() -> u64 {
    let size = std::env::var("SQRY_MMAP_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MMAP_THRESHOLD);
    size.clamp(MIN_MMAP_THRESHOLD, MAX_MMAP_THRESHOLD)
}

/// Get maximum source file size, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_MAX_SOURCE_FILE_SIZE` environment variable.
/// If not set or invalid, returns [`DEFAULT_MAX_SOURCE_FILE_SIZE`].
/// Values are clamped between 1 MB and 500 MB for safety (RR-10 `DoS` prevention).
///
/// # `DoS` Prevention
///
/// Limits maximum file size for indexing to prevent memory exhaustion attacks
/// via repositories containing multi-gigabyte source files. Files exceeding this
/// limit are rejected with a clear error message during indexing.
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::max_source_file_size;
///
/// let max_size = max_source_file_size();
/// assert!(max_size >= 1048576); // At least 1 MB
/// assert!(max_size <= 524288000); // At most 500 MB
/// ```
#[must_use]
pub fn max_source_file_size() -> u64 {
    let size = std::env::var("SQRY_MAX_SOURCE_FILE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_SOURCE_FILE_SIZE);
    size.clamp(MIN_MAX_SOURCE_FILE_SIZE, MAX_MAX_SOURCE_FILE_SIZE)
}

/// Get maximum repositories per workspace, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_MAX_REPOSITORIES` environment variable.
/// If not set or invalid, returns [`DEFAULT_MAX_REPOSITORIES`].
/// Values are clamped between 10 and 10,000 for safety (RR-10 `DoS` prevention).
///
/// # `DoS` Prevention
///
/// Limits the number of repositories discovered during workspace scanning to prevent
/// memory exhaustion attacks via workspaces containing thousands of `.sqry-index` files.
/// Workspaces exceeding this limit are rejected with a clear error message.
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::max_repositories;
///
/// let max_repos = max_repositories();
/// assert!(max_repos >= 10); // At least 10
/// assert!(max_repos <= 10000); // At most 10,000
/// ```
#[must_use]
pub fn max_repositories() -> usize {
    let count = std::env::var("SQRY_MAX_REPOSITORIES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_REPOSITORIES);
    count.clamp(MIN_MAX_REPOSITORIES, MAX_MAX_REPOSITORIES)
}

/// Maximum file watcher event queue capacity (10,000 events)
///
/// Limits the number of queued filesystem events in the file watcher channel
/// to prevent `DoS` attacks via event flooding (RR-10 Gap #3).
pub const DEFAULT_WATCH_EVENT_QUEUE: usize = 10_000;

// Safety bounds for watch event queue (RR-10: DoS prevention)
const MIN_WATCH_EVENT_QUEUE: usize = 100; // 100 minimum (small workspaces)
const MAX_WATCH_EVENT_QUEUE: usize = 100_000; // 100k maximum (large workspaces with many watchers)

/// Get watch event queue capacity, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_WATCH_EVENT_QUEUE` environment variable.
/// If not set or invalid, returns [`DEFAULT_WATCH_EVENT_QUEUE`].
/// Values are clamped between 100 and 100,000 for safety (RR-10 `DoS` prevention).
///
/// # `DoS` Prevention
///
/// Limits the number of filesystem events that can be queued in the file watcher
/// to prevent memory exhaustion attacks via event flooding. When the queue is full,
/// the watcher applies backpressure by blocking new events until existing ones are
/// processed, preventing unbounded memory growth.
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::watch_event_queue_capacity;
///
/// let capacity = watch_event_queue_capacity();
/// assert!(capacity >= 100); // At least 100
/// assert!(capacity <= 100000); // At most 100,000
/// ```
#[must_use]
pub fn watch_event_queue_capacity() -> usize {
    let capacity = std::env::var("SQRY_WATCH_EVENT_QUEUE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WATCH_EVENT_QUEUE);
    capacity.clamp(MIN_WATCH_EVENT_QUEUE, MAX_WATCH_EVENT_QUEUE)
}

/// Maximum query string length (10 KB)
///
/// Limits the length of query strings to prevent `DoS` attacks via
/// extremely long queries that consume excessive parsing time and memory (RR-10 Gap #4).
pub const DEFAULT_MAX_QUERY_LENGTH: usize = 10 * 1024; // 10 KB

// Safety bounds for query length (RR-10: DoS prevention)
const MIN_MAX_QUERY_LENGTH: usize = 1024; // 1 KB minimum (reasonable for complex queries)
const MAX_MAX_QUERY_LENGTH: usize = 100 * 1024; // 100 KB maximum (extremely complex queries)

/// Get maximum query string length, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_MAX_QUERY_LENGTH` environment variable.
/// If not set or invalid, returns [`DEFAULT_MAX_QUERY_LENGTH`].
/// Values are clamped between 1 KB and 100 KB for safety (RR-10 `DoS` prevention).
///
/// # `DoS` Prevention
///
/// Limits the maximum length of query strings to prevent memory exhaustion and
/// excessive CPU usage from parsing extremely long queries. Queries exceeding this
/// limit are rejected with a clear error message.
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::max_query_length;
///
/// let max_len = max_query_length();
/// assert!(max_len >= 1024); // At least 1 KB
/// assert!(max_len <= 102400); // At most 100 KB
/// ```
#[must_use]
pub fn max_query_length() -> usize {
    let length = std::env::var("SQRY_MAX_QUERY_LENGTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_QUERY_LENGTH);
    length.clamp(MIN_MAX_QUERY_LENGTH, MAX_MAX_QUERY_LENGTH)
}

/// Maximum number of predicates per query (100)
///
/// Limits the number of predicates in a query to prevent `DoS` attacks via
/// queries with thousands of predicates that consume excessive memory and CPU (RR-10 Gap #4).
pub const DEFAULT_MAX_PREDICATES: usize = 100;

// Safety bounds for predicate count (RR-10: DoS prevention)
const MIN_MAX_PREDICATES: usize = 10; // 10 minimum (reasonable for basic queries)
const MAX_MAX_PREDICATES: usize = 1000; // 1000 maximum (very complex queries)

/// Get maximum predicates per query, respecting environment variable override
///
/// # Environment
///
/// Reads from `SQRY_MAX_PREDICATES` environment variable.
/// If not set or invalid, returns [`DEFAULT_MAX_PREDICATES`].
/// Values are clamped between 10 and 1,000 for safety (RR-10 `DoS` prevention).
///
/// # `DoS` Prevention
///
/// Limits the number of predicates in a query to prevent memory exhaustion and
/// excessive CPU usage from evaluating queries with thousands of predicates.
/// Queries exceeding this limit are rejected with a clear error message.
///
/// # Examples
///
/// ```
/// use sqry_core::config::buffers::max_predicates;
///
/// let max_preds = max_predicates();
/// assert!(max_preds >= 10); // At least 10
/// assert!(max_preds <= 1000); // At most 1,000
/// ```
#[must_use]
pub fn max_predicates() -> usize {
    let count = std::env::var("SQRY_MAX_PREDICATES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_PREDICATES);
    count.clamp(MIN_MAX_PREDICATES, MAX_MAX_PREDICATES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn test_default_read_buffer_size() {
        unsafe {
            std::env::remove_var("SQRY_READ_BUFFER");
        }
        assert_eq!(read_buffer_size(), DEFAULT_READ_BUFFER);
    }

    #[test]
    #[serial]
    fn test_env_override_read_buffer() {
        unsafe {
            std::env::set_var("SQRY_READ_BUFFER", "16384");
        }
        assert_eq!(read_buffer_size(), 16384);
        unsafe {
            std::env::remove_var("SQRY_READ_BUFFER");
        }
    }

    #[test]
    #[serial]
    fn test_invalid_env_uses_default() {
        unsafe {
            std::env::set_var("SQRY_READ_BUFFER", "invalid");
        }
        assert_eq!(read_buffer_size(), DEFAULT_READ_BUFFER);
        unsafe {
            std::env::remove_var("SQRY_READ_BUFFER");
        }
    }

    #[test]
    fn test_all_buffer_sizes_present() {
        // Verify all constants defined
        let _ = DEFAULT_READ_BUFFER;
        let _ = DEFAULT_WRITE_BUFFER;
        let _ = DEFAULT_PARSE_BUFFER;
        let _ = DEFAULT_INDEX_BUFFER;
    }

    // Compile-time assertions: Sanity checks for buffer size constants
    const _: () = assert!(DEFAULT_READ_BUFFER >= 4096, "Read buffer too small");
    const _: () = assert!(DEFAULT_READ_BUFFER <= 65536, "Read buffer too large");
    const _: () = assert!(DEFAULT_INDEX_BUFFER >= 65536, "Index buffer too small");

    #[test]
    #[serial]
    fn test_write_buffer_size() {
        unsafe {
            std::env::remove_var("SQRY_WRITE_BUFFER");
        }
        assert_eq!(write_buffer_size(), DEFAULT_WRITE_BUFFER);
    }

    #[test]
    #[serial]
    fn test_parse_buffer_size() {
        unsafe {
            std::env::remove_var("SQRY_PARSE_BUFFER");
        }
        assert_eq!(parse_buffer_size(), DEFAULT_PARSE_BUFFER);
    }

    #[test]
    #[serial]
    fn test_index_buffer_size() {
        unsafe {
            std::env::remove_var("SQRY_INDEX_BUFFER");
        }
        assert_eq!(index_buffer_size(), DEFAULT_INDEX_BUFFER);
    }

    #[test]
    #[serial]
    fn test_env_override_write_buffer() {
        unsafe {
            std::env::set_var("SQRY_WRITE_BUFFER", "32768");
        }
        assert_eq!(write_buffer_size(), 32768);
        unsafe {
            std::env::remove_var("SQRY_WRITE_BUFFER");
        }
    }

    #[test]
    #[serial]
    fn test_env_override_parse_buffer() {
        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "131072");
        }
        assert_eq!(parse_buffer_size(), 131_072);
        unsafe {
            std::env::remove_var("SQRY_PARSE_BUFFER");
        }
    }

    #[test]
    #[serial]
    fn test_env_override_index_buffer() {
        unsafe {
            std::env::set_var("SQRY_INDEX_BUFFER", "2097152");
        }
        assert_eq!(index_buffer_size(), 2_097_152);
        unsafe {
            std::env::remove_var("SQRY_INDEX_BUFFER");
        }
    }

    #[test]
    #[serial]
    fn test_parse_buffer_used_in_hash_file() {
        // Integration test: Verify hash_file() respects SQRY_PARSE_BUFFER
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create test file
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = vec![b'X'; 200_000]; // 200KB file
        temp_file.write_all(&test_data).unwrap();
        temp_file.flush().unwrap();

        // Test with custom buffer size
        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "32768"); // 32KB
        }
        assert_eq!(parse_buffer_size(), 32768);

        // Hash file should work and use the env var (validated by not panicking)
        let hash1 = crate::hash::hash_file(temp_file.path()).unwrap();

        // Test with different buffer size - should produce same hash
        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "16384"); // 16KB
        }
        assert_eq!(parse_buffer_size(), 16384);
        let hash2 = crate::hash::hash_file(temp_file.path()).unwrap();

        // Hashes should be identical regardless of buffer size
        assert_eq!(hash1, hash2, "Hash should be independent of buffer size");

        unsafe {
            std::env::remove_var("SQRY_PARSE_BUFFER");
        }
    }

    #[test]
    #[serial]
    fn test_buffer_clamping() {
        // Test read buffer clamping
        unsafe {
            std::env::set_var("SQRY_READ_BUFFER", "500"); // Below minimum
        }
        assert_eq!(read_buffer_size(), 1024); // Clamped to minimum

        unsafe {
            std::env::set_var("SQRY_READ_BUFFER", "5000000"); // Above maximum
        }
        assert_eq!(read_buffer_size(), 1_048_576); // Clamped to maximum

        // Test parse buffer clamping
        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "1000"); // Below minimum
        }
        assert_eq!(parse_buffer_size(), 4096); // Clamped to minimum

        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "50000000"); // Above maximum
        }
        assert_eq!(parse_buffer_size(), 10_485_760); // Clamped to maximum

        // Cleanup
        unsafe {
            std::env::remove_var("SQRY_READ_BUFFER");
            std::env::remove_var("SQRY_PARSE_BUFFER");
        }
    }

    #[test]
    #[serial]
    fn test_parse_buffer_used_in_incremental() {
        // Integration test: Verify FileHash::compute() respects SQRY_PARSE_BUFFER
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create test file
        let mut temp_file = NamedTempFile::new().unwrap();
        let test_data = vec![b'Y'; 150_000]; // 150KB file
        temp_file.write_all(&test_data).unwrap();
        temp_file.flush().unwrap();

        // Test with custom buffer size
        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "65536"); // 64KB
        }
        assert_eq!(parse_buffer_size(), 65536);

        // Compute hash should work and use the env var
        let hash1 = crate::indexing::FileHash::compute(temp_file.path()).unwrap();

        // Test with different buffer size - should produce same hash
        unsafe {
            std::env::set_var("SQRY_PARSE_BUFFER", "8192"); // 8KB
        }
        assert_eq!(parse_buffer_size(), 8192);
        let hash2 = crate::indexing::FileHash::compute(temp_file.path()).unwrap();

        // Hashes should be identical regardless of buffer size
        assert_eq!(
            hash1.hash, hash2.hash,
            "FileHash should be independent of buffer size"
        );

        unsafe {
            std::env::remove_var("SQRY_PARSE_BUFFER");
        }
    }

    // P1-17: Mmap threshold tests
    #[test]
    #[serial]
    fn test_default_mmap_threshold() {
        unsafe {
            std::env::remove_var("SQRY_MMAP_THRESHOLD");
        }
        assert_eq!(mmap_threshold(), DEFAULT_MMAP_THRESHOLD);
    }

    #[test]
    #[serial]
    fn test_env_override_mmap_threshold() {
        unsafe {
            std::env::set_var("SQRY_MMAP_THRESHOLD", "52428800"); // 50MB
        }
        assert_eq!(mmap_threshold(), 52_428_800);
        unsafe {
            std::env::remove_var("SQRY_MMAP_THRESHOLD");
        }
    }

    #[test]
    #[serial]
    fn test_mmap_threshold_clamping() {
        // Test below minimum
        unsafe {
            std::env::set_var("SQRY_MMAP_THRESHOLD", "500000"); // 500KB - below 1MB minimum
        }
        assert_eq!(mmap_threshold(), MIN_MMAP_THRESHOLD); // Clamped to 1MB

        // Test above maximum
        unsafe {
            std::env::set_var("SQRY_MMAP_THRESHOLD", "2147483648"); // 2GB - above 1GB maximum
        }
        assert_eq!(mmap_threshold(), MAX_MMAP_THRESHOLD); // Clamped to 1GB

        // Cleanup
        unsafe {
            std::env::remove_var("SQRY_MMAP_THRESHOLD");
        }
    }

    #[test]
    #[serial]
    fn test_mmap_threshold_malformed() {
        unsafe {
            std::env::set_var("SQRY_MMAP_THRESHOLD", "not_a_number");
        }
        assert_eq!(mmap_threshold(), DEFAULT_MMAP_THRESHOLD);
        unsafe {
            std::env::remove_var("SQRY_MMAP_THRESHOLD");
        }
    }
}
