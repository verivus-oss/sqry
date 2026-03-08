//! Efficient file reading abstraction with memory-mapping support
//!
//! Provides `FileReader` that automatically chooses between memory-mapped
//! and buffered reading based on file size and platform capabilities.

use anyhow::{Context, Result, bail};
use memmap2::Mmap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

// P1-17: Use configurable mmap threshold from config::buffers
// RR-10: Use configurable max file size for DoS prevention
use crate::config::buffers::{max_source_file_size, mmap_threshold};

/// Policy for choosing file reading strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReaderPolicy {
    /// Always use buffered reading
    Buffered,
    /// Always attempt memory-mapping (fallback to buffered on failure)
    Mmap,
    /// Automatically choose based on file size
    Auto {
        /// Threshold in bytes for switching to mmap
        threshold: u64,
    },
}

impl Default for ReaderPolicy {
    fn default() -> Self {
        // P1-17: Use configurable threshold (respects SQRY_MMAP_THRESHOLD)
        Self::Auto {
            threshold: mmap_threshold(),
        }
    }
}

/// File reader that supports both memory-mapped and buffered reading
pub enum FileReader {
    /// Memory-mapped file
    Mmap {
        /// File handle (kept alive to ensure mmap validity)
        #[allow(dead_code)]
        file: File,
        /// Memory-mapped region
        mmap: Mmap,
    },
    /// Buffered file data
    Buffered {
        /// File contents loaded into memory
        data: Vec<u8>,
    },
}

impl FileReader {
    /// Open a file using the default policy (auto with 10MB threshold)
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the file cannot be opened, memory-mapped, or read.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_with_policy(path, ReaderPolicy::default())
    }

    /// Open a file with a specific reading policy
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when file metadata access, mmap, or buffered reads fail.
    /// Also returns error if file exceeds the maximum source file size limit (RR-10 `DoS` prevention).
    pub fn open_with_policy<P: AsRef<Path>>(path: P, policy: ReaderPolicy) -> Result<Self> {
        let path = path.as_ref();
        let file =
            File::open(path).with_context(|| format!("Failed to open file: {}", path.display()))?;

        let metadata = file
            .metadata()
            .with_context(|| format!("Failed to read file metadata: {}", path.display()))?;

        let file_size = metadata.len();

        // RR-10 Gap #1: Enforce maximum file size to prevent DoS via huge files
        let max_size = max_source_file_size();
        if file_size > max_size {
            bail!(
                "File too large to index: {} ({} MB exceeds {} MB limit). \
                 Adjust SQRY_MAX_SOURCE_FILE_SIZE environment variable if needed.",
                path.display(),
                file_size / (1024 * 1024),
                max_size / (1024 * 1024)
            );
        }

        // Decide reading strategy
        let use_mmap = match policy {
            ReaderPolicy::Buffered => false,
            ReaderPolicy::Mmap => true,
            ReaderPolicy::Auto { threshold } => file_size >= threshold,
        };

        if use_mmap {
            // Try memory-mapping first
            match Self::try_mmap(file, path) {
                Ok(reader) => Ok(reader),
                Err(_e) => {
                    // Mmap failed, fallback to buffered reading
                    // Reopen file since we consumed it
                    let mut file = File::open(path)?;
                    Self::read_buffered(&mut file, path)
                }
            }
        } else {
            let mut file_for_read = file;
            Self::read_buffered(&mut file_for_read, path)
        }
    }

    /// Attempt to create a memory-mapped reader
    fn try_mmap(file: File, path: &Path) -> Result<Self> {
        // Safety: We're only reading the file, and we keep the File handle alive
        // to ensure the mapping remains valid
        let mmap = unsafe {
            Mmap::map(&file).with_context(|| format!("Failed to mmap file: {}", path.display()))?
        };

        Ok(FileReader::Mmap { file, mmap })
    }

    /// Read file contents into a buffer
    fn read_buffered(file: &mut File, path: &Path) -> Result<Self> {
        let mut data = Vec::new();
        file.read_to_end(&mut data)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;

        Ok(FileReader::Buffered { data })
    }

    /// Get a slice of the file contents
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        match self {
            FileReader::Mmap { mmap, .. } => &mmap[..],
            FileReader::Buffered { data } => &data[..],
        }
    }

    /// Get the size of the file in bytes
    #[must_use]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    /// Check if the file is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over chunks of the file
    pub fn chunks(&self, chunk_size: usize) -> impl Iterator<Item = &[u8]> {
        self.as_slice().chunks(chunk_size)
    }
}

impl AsRef<[u8]> for FileReader {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_temp_file(size: usize) -> (NamedTempFile, Vec<u8>) {
        let mut file = NamedTempFile::new().expect("Failed to create temp file");
        // Modulo 256 ensures value fits in u8; safe cast
        #[allow(clippy::cast_possible_truncation)]
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        file.write_all(&data).expect("Failed to write temp file");
        file.flush().expect("Failed to flush temp file");
        (file, data)
    }

    #[test]
    fn test_buffered_small_file() {
        let (file, expected_data) = create_temp_file(1024);

        let reader = FileReader::open_with_policy(file.path(), ReaderPolicy::Buffered)
            .expect("Failed to open file");

        assert_eq!(reader.as_slice(), &expected_data[..]);
        assert_eq!(reader.len(), 1024);
        assert!(!reader.is_empty());
    }

    #[test]
    fn test_mmap_large_file() {
        let size = 15 * 1024 * 1024; // 15 MB
        let (file, expected_data) = create_temp_file(size);

        let reader = FileReader::open_with_policy(file.path(), ReaderPolicy::Mmap)
            .expect("Failed to open file");

        assert_eq!(reader.as_slice(), &expected_data[..]);
        assert_eq!(reader.len(), size);
    }

    #[test]
    fn test_auto_policy_small_file() {
        let (file, expected_data) = create_temp_file(1024);

        let reader = FileReader::open_with_policy(
            file.path(),
            ReaderPolicy::Auto {
                threshold: 10 * 1024 * 1024,
            },
        )
        .expect("Failed to open file");

        assert_eq!(reader.as_slice(), &expected_data[..]);
    }

    #[test]
    fn test_auto_policy_large_file() {
        let size = 15 * 1024 * 1024; // 15 MB
        let (file, expected_data) = create_temp_file(size);

        let reader = FileReader::open_with_policy(
            file.path(),
            ReaderPolicy::Auto {
                threshold: 10 * 1024 * 1024,
            },
        )
        .expect("Failed to open file");

        assert_eq!(reader.as_slice(), &expected_data[..]);
        assert_eq!(reader.len(), size);
    }

    #[test]
    fn test_chunks_iteration() {
        let (file, _) = create_temp_file(1000);

        let reader = FileReader::open(file.path()).expect("Failed to open file");

        let chunks: Vec<_> = reader.chunks(100).collect();
        assert_eq!(chunks.len(), 10);
        assert_eq!(chunks[0].len(), 100);
        assert_eq!(chunks[9].len(), 100);
    }

    #[test]
    fn test_empty_file() {
        let file = NamedTempFile::new().expect("Failed to create temp file");

        let reader = FileReader::open(file.path()).expect("Failed to open file");

        assert!(reader.is_empty());
        assert_eq!(reader.len(), 0);
    }

    #[test]
    fn test_threshold_boundary() {
        let threshold = 5 * 1024; // 5 KB

        // Test file sizes bounded by realistic test data (5KB±1); safe to convert
        // Just below threshold
        let (file_small, data_small) =
            create_temp_file(threshold.try_into().unwrap_or(usize::MAX).saturating_sub(1));
        let reader_small =
            FileReader::open_with_policy(file_small.path(), ReaderPolicy::Auto { threshold })
                .expect("Failed to open small file");
        assert_eq!(reader_small.as_slice(), &data_small[..]);

        // At threshold
        let (file_exact, data_exact) = create_temp_file(threshold.try_into().unwrap_or(usize::MAX));
        let reader_exact =
            FileReader::open_with_policy(file_exact.path(), ReaderPolicy::Auto { threshold })
                .expect("Failed to open exact file");
        assert_eq!(reader_exact.as_slice(), &data_exact[..]);

        // Just above threshold
        let (file_large, data_large) =
            create_temp_file(threshold.try_into().unwrap_or(usize::MAX).saturating_add(1));
        let reader_large =
            FileReader::open_with_policy(file_large.path(), ReaderPolicy::Auto { threshold })
                .expect("Failed to open large file");
        assert_eq!(reader_large.as_slice(), &data_large[..]);
    }
}
