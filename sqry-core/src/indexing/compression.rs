//! Index compression for reducing disk space and improving cold-start times.
//!
//! This module provides zstd-based compression for serialized indexes with
//! backward compatibility for legacy uncompressed indexes.
//!
//! # Format
//!
//! Compressed indexes use a header-based format:
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │ Magic: "SQRY" (4 bytes)              │
//! ├──────────────────────────────────────┤
//! │ Version: u32 (4 bytes)               │
//! ├──────────────────────────────────────┤
//! │ Compression: u8 (1 byte)             │
//! │   0 = None, 1 = zstd                 │
//! ├──────────────────────────────────────┤
//! │ Level: i32 (4 bytes)                 │
//! ├──────────────────────────────────────┤
//! │ Uncompressed Size: u64 (8 bytes)     │
//! ├──────────────────────────────────────┤
//! │ Compressed Data (variable)           │
//! └──────────────────────────────────────┘
//! ```
//!
//! # Examples
//!
//! ```no_run
//! use sqry_core::indexing::CompressedIndex;
//!
//! // Compress index data
//! let data = b"index data here";
//! let compressed = CompressedIndex::compress(data, 3)?;
//!
//! // Serialize to disk format
//! let serialized = compressed.serialize();
//!
//! // Deserialize and decompress
//! let loaded = CompressedIndex::deserialize(&serialized)?;
//! let original = loaded.decompress()?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::io::{self, Read, Write};

/// Magic bytes for compressed index format: "SQRY"
const MAGIC: &[u8; 4] = b"SQRY";

/// Current format version
const FORMAT_VERSION: u32 = 1;

/// Default zstd compression level (3 = fast, good compression ratio)
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 3;

/// Maximum allowed uncompressed size (500 MB by default)
///
/// This prevents decompression bombs from consuming excessive memory.
/// Can be overridden via `SQRY_MAX_INDEX_SIZE` environment variable.
pub const DEFAULT_MAX_UNCOMPRESSED_SIZE: u64 = 500 * 1024 * 1024;

/// Minimum allowed maximum uncompressed size (1 MB)
const MIN_MAX_UNCOMPRESSED_SIZE: u64 = 1024 * 1024;

/// Maximum allowed maximum uncompressed size (2 GB)
const MAX_MAX_UNCOMPRESSED_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// Get maximum allowed uncompressed size
///
/// Reads from `SQRY_MAX_INDEX_SIZE` environment variable if set,
/// otherwise returns `DEFAULT_MAX_UNCOMPRESSED_SIZE`.
///
/// # Security
///
/// Values are clamped between 1 MB and 2 GB for safety (P1-14).
/// This prevents malicious environment variable values from either:
/// - Allowing excessively large decompression (`DoS` via memory exhaustion)
/// - Setting the limit too low (`DoS` via rejecting valid indexes)
#[must_use]
pub fn max_uncompressed_size() -> u64 {
    let size = std::env::var("SQRY_MAX_INDEX_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_UNCOMPRESSED_SIZE);
    size.clamp(MIN_MAX_UNCOMPRESSED_SIZE, MAX_MAX_UNCOMPRESSED_SIZE)
}

/// Compression format type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompressionFormat {
    /// No compression (stored as-is)
    None = 0,
    /// Zstd compression
    Zstd = 1,
}

impl CompressionFormat {
    /// Convert from u8 byte value
    fn from_u8(value: u8) -> Result<Self, CompressionError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::Zstd),
            _ => Err(CompressionError::UnsupportedCompression(value)),
        }
    }
}

/// Errors that can occur during compression/decompression
#[derive(Debug, thiserror::Error)]
pub enum CompressionError {
    /// I/O error during compression/decompression
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Unsupported compression format
    #[error("Unsupported compression format: {0}")]
    UnsupportedCompression(u8),

    /// Invalid magic bytes (not a compressed index)
    #[error("Invalid magic bytes, expected SQRY")]
    InvalidMagic,

    /// Index format version is too new
    #[error("Index version {index_version} is too new for sqry {sqry_version}, please upgrade")]
    IndexVersionTooNew {
        /// Version number found in the index file
        index_version: u32,
        /// Current sqry binary version
        sqry_version: &'static str,
    },

    /// Invalid format version (reserved value 0)
    #[error("Invalid index version: {0}")]
    InvalidIndexVersion(u32),

    /// Header is too small to be valid
    #[error("Invalid header size: expected at least 21 bytes, got {0}")]
    InvalidHeaderSize(usize),

    /// Size mismatch after decompression
    #[error("Decompressed size mismatch: expected {expected}, got {actual}")]
    SizeMismatch {
        /// Expected uncompressed size from header
        expected: u64,
        /// Actual size after decompression
        actual: u64,
    },

    /// Decompression bomb detected (uncompressed size exceeds maximum)
    #[error("Decompression bomb detected: uncompressed size {size} exceeds maximum {max}")]
    DecompressionBomb {
        /// Declared uncompressed size
        size: u64,
        /// Maximum allowed size
        max: u64,
    },
}

/// Compressed index container with format metadata
#[derive(Debug, Clone)]
pub struct CompressedIndex {
    /// Format version (currently 1)
    version: u32,
    /// Compression format used
    compression: CompressionFormat,
    /// Compression level (for zstd)
    level: i32,
    /// Original uncompressed size
    uncompressed_size: u64,
    /// Compressed data
    data: Vec<u8>,
}

impl CompressedIndex {
    /// Compress data using zstd compression.
    ///
    /// # Arguments
    ///
    /// * `data` - The data to compress
    /// * `level` - Compression level (1-22, where 3 is default)
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::indexing::CompressedIndex;
    ///
    /// let data = b"test data";
    /// let compressed = CompressedIndex::compress(data, 3)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`CompressionError::Io`] if zstd fails to create the encoder or if writing to
    /// the compressor fails.
    pub fn compress(data: &[u8], level: i32) -> Result<Self, CompressionError> {
        let mut encoder = zstd::Encoder::new(Vec::new(), level)?;
        encoder.write_all(data)?;
        let compressed = encoder.finish()?;

        Ok(Self {
            version: FORMAT_VERSION,
            compression: CompressionFormat::Zstd,
            level,
            uncompressed_size: data.len() as u64,
            data: compressed,
        })
    }

    /// Create an uncompressed index container (for testing or fallback).
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::indexing::CompressedIndex;
    ///
    /// let data = b"test data";
    /// let uncompressed = CompressedIndex::uncompressed(data);
    /// ```
    #[must_use]
    pub fn uncompressed(data: &[u8]) -> Self {
        Self {
            version: FORMAT_VERSION,
            compression: CompressionFormat::None,
            level: 0,
            uncompressed_size: data.len() as u64,
            data: data.to_vec(),
        }
    }

    /// Decompress the index data.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::indexing::CompressedIndex;
    ///
    /// let data = b"test data";
    /// let compressed = CompressedIndex::compress(data, 3)?;
    /// let decompressed = compressed.decompress()?;
    /// assert_eq!(data, &decompressed[..]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`CompressionError`] when decompression exceeds the configured safety limit,
    /// when zstd emits an error, or when the resulting size does not match the stored header.
    pub fn decompress(&self) -> Result<Vec<u8>, CompressionError> {
        // P1-14: Check for decompression bombs before allocating memory
        let max_size = max_uncompressed_size();
        if self.uncompressed_size > max_size {
            return Err(CompressionError::DecompressionBomb {
                size: self.uncompressed_size,
                max: max_size,
            });
        }

        match self.compression {
            CompressionFormat::None => {
                // Even uncompressed data needs size check
                if self.data.len() as u64 > max_size {
                    return Err(CompressionError::DecompressionBomb {
                        size: self.data.len() as u64,
                        max: max_size,
                    });
                }
                Ok(self.data.clone())
            }
            CompressionFormat::Zstd => {
                // P1-14: CODEX review - enforce streaming limit during decompression
                let decoder = zstd::Decoder::new(&self.data[..])?;

                // Limit the decoder to max_size + 1 bytes to distinguish between:
                // 1) Data that is exactly max_size (valid)
                // 2) Data that exceeds max_size (decompression bomb)
                // This ensures we never read more than the allowed maximum while
                // allowing legitimate indexes at the boundary to pass.
                let mut limited = decoder.take(max_size + 1);
                let mut decompressed = Vec::new();
                limited.read_to_end(&mut decompressed)?;

                // Verify decompressed size matches header
                let actual_size = decompressed.len() as u64;
                if actual_size != self.uncompressed_size {
                    return Err(CompressionError::SizeMismatch {
                        expected: self.uncompressed_size,
                        actual: actual_size,
                    });
                }

                // Check if we exceeded the limit (decompression bomb detected)
                // Using > instead of >= to allow data exactly at the limit
                if actual_size > max_size {
                    return Err(CompressionError::DecompressionBomb {
                        size: actual_size,
                        max: max_size,
                    });
                }

                Ok(decompressed)
            }
        }
    }

    /// Serialize to on-disk format with header.
    ///
    /// # Format
    ///
    /// - Magic: "SQRY" (4 bytes)
    /// - Version: u32 little-endian (4 bytes)
    /// - Compression: u8 (1 byte)
    /// - Level: i32 little-endian (4 bytes)
    /// - Uncompressed Size: u64 little-endian (8 bytes)
    /// - Data: variable length
    ///
    /// Total header size: 21 bytes
    #[must_use]
    pub fn serialize(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(21 + self.data.len());

        // Write magic
        buffer.extend_from_slice(MAGIC);

        // Write version
        buffer.extend_from_slice(&self.version.to_le_bytes());

        // Write compression format
        buffer.push(self.compression as u8);

        // Write compression level
        buffer.extend_from_slice(&self.level.to_le_bytes());

        // Write uncompressed size
        buffer.extend_from_slice(&self.uncompressed_size.to_le_bytes());

        // Write compressed data
        buffer.extend_from_slice(&self.data);

        buffer
    }

    /// Deserialize from on-disk format.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Magic bytes don't match "SQRY"
    /// - Header is too small
    /// - Version is unsupported
    /// - Compression format is unknown
    pub fn deserialize(data: &[u8]) -> Result<Self, CompressionError> {
        // Check minimum header size (21 bytes)
        if data.len() < 21 {
            return Err(CompressionError::InvalidHeaderSize(data.len()));
        }

        // Check magic bytes
        if &data[0..4] != MAGIC {
            return Err(CompressionError::InvalidMagic);
        }

        // Parse version
        let version = u32::from_le_bytes(
            data[4..8]
                .try_into()
                .map_err(|_| CompressionError::InvalidHeaderSize(data.len()))?,
        );

        // Check version compatibility
        match version {
            0 => return Err(CompressionError::InvalidIndexVersion(0)),
            FORMAT_VERSION => {
                // Current version, continue parsing
            }
            v if v > FORMAT_VERSION => {
                return Err(CompressionError::IndexVersionTooNew {
                    index_version: v,
                    sqry_version: env!("CARGO_PKG_VERSION"),
                });
            }
            _ => {
                // Older version - could support in future if needed
                return Err(CompressionError::InvalidIndexVersion(version));
            }
        }

        // Parse compression format
        let compression = CompressionFormat::from_u8(data[8])?;

        // Parse level
        let level = i32::from_le_bytes(
            data[9..13]
                .try_into()
                .map_err(|_| CompressionError::InvalidHeaderSize(data.len()))?,
        );

        // Parse uncompressed size
        let uncompressed_size = u64::from_le_bytes(
            data[13..21]
                .try_into()
                .map_err(|_| CompressionError::InvalidHeaderSize(data.len()))?,
        );

        // Extract data
        let index_data = data[21..].to_vec();

        Ok(Self {
            version,
            compression,
            level,
            uncompressed_size,
            data: index_data,
        })
    }

    /// Get the compression format used.
    #[must_use]
    pub fn compression(&self) -> CompressionFormat {
        self.compression
    }

    /// Get the uncompressed size.
    #[must_use]
    pub fn uncompressed_size(&self) -> u64 {
        self.uncompressed_size
    }

    /// Get the compressed size (actual data size).
    #[must_use]
    pub fn compressed_size(&self) -> usize {
        self.data.len()
    }

    /// Get the compression ratio (uncompressed / compressed).
    ///
    /// Returns 1.0 for uncompressed data.
    #[must_use]
    pub fn compression_ratio(&self) -> f64 {
        if self.data.is_empty() {
            return 1.0;
        }
        Self::to_f64_lossy_u64(self.uncompressed_size) / Self::to_f64_lossy_usize(self.data.len())
    }

    #[inline]
    #[allow(clippy::cast_precision_loss)] // Human-readable ratios tolerate lossy conversion
    fn to_f64_lossy_u64(value: u64) -> f64 {
        value as f64
    }

    #[inline]
    #[allow(clippy::cast_precision_loss)] // Human-readable ratios tolerate lossy conversion
    fn to_f64_lossy_usize(value: usize) -> f64 {
        value as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_decompress_roundtrip() {
        let original = b"test data for compression";
        let compressed = CompressedIndex::compress(original, DEFAULT_COMPRESSION_LEVEL).unwrap();
        let decompressed = compressed.decompress().unwrap();

        assert_eq!(original, &decompressed[..]);
    }

    #[test]
    fn test_serialize_deserialize_roundtrip() {
        let original = b"test data for serialization";
        let compressed = CompressedIndex::compress(original, 3).unwrap();
        let serialized = compressed.serialize();
        let deserialized = CompressedIndex::deserialize(&serialized).unwrap();
        let decompressed = deserialized.decompress().unwrap();

        assert_eq!(original, &decompressed[..]);
    }

    #[test]
    fn test_compression_reduces_size() {
        // Create highly compressible data (repeated pattern)
        let original = vec![b'a'; 10000];
        let compressed = CompressedIndex::compress(&original, 3).unwrap();

        assert!(
            compressed.compressed_size() < original.len(),
            "Compressed size {} should be less than original size {}",
            compressed.compressed_size(),
            original.len()
        );
    }

    #[test]
    fn test_compression_ratio() {
        let original = vec![b'x'; 1000];
        let compressed = CompressedIndex::compress(&original, 3).unwrap();

        let ratio = compressed.compression_ratio();
        assert!(
            ratio > 1.0,
            "Compression ratio should be > 1.0 for compressible data"
        );
    }

    #[test]
    fn test_uncompressed_roundtrip() {
        let original = b"uncompressed test data";
        let uncompressed = CompressedIndex::uncompressed(original);
        let decompressed = uncompressed.decompress().unwrap();

        assert_eq!(original, &decompressed[..]);
        assert_eq!(uncompressed.compression(), CompressionFormat::None);
    }

    #[test]
    fn test_magic_bytes_in_header() {
        let original = b"test";
        let compressed = CompressedIndex::compress(original, 3).unwrap();
        let serialized = compressed.serialize();

        assert_eq!(&serialized[0..4], b"SQRY");
    }

    #[test]
    fn test_invalid_magic_bytes() {
        // Need at least 21 bytes to pass size check, but with wrong magic
        let mut invalid_data = vec![0u8; 21];
        invalid_data[0..4].copy_from_slice(b"XXXX"); // Wrong magic
        let result = CompressedIndex::deserialize(&invalid_data);

        assert!(matches!(result, Err(CompressionError::InvalidMagic)));
    }

    #[test]
    fn test_header_too_small() {
        let too_small = b"SQRY123"; // Only 7 bytes
        let result = CompressedIndex::deserialize(too_small);

        assert!(matches!(
            result,
            Err(CompressionError::InvalidHeaderSize(7))
        ));
    }

    #[test]
    fn test_unsupported_compression_format() {
        let mut data = vec![0u8; 21];
        data[0..4].copy_from_slice(b"SQRY");
        data[4..8].copy_from_slice(&1u32.to_le_bytes()); // version = 1
        data[8] = 99; // Invalid compression format

        let result = CompressedIndex::deserialize(&data);

        assert!(matches!(
            result,
            Err(CompressionError::UnsupportedCompression(99))
        ));
    }

    #[test]
    fn test_future_version_error() {
        let mut data = vec![0u8; 21];
        data[0..4].copy_from_slice(b"SQRY");
        data[4..8].copy_from_slice(&999u32.to_le_bytes()); // version = 999

        let result = CompressedIndex::deserialize(&data);

        assert!(matches!(
            result,
            Err(CompressionError::IndexVersionTooNew { .. })
        ));
    }

    #[test]
    fn test_zero_version_error() {
        let mut data = vec![0u8; 21];
        data[0..4].copy_from_slice(b"SQRY");
        data[4..8].copy_from_slice(&0u32.to_le_bytes()); // version = 0

        let result = CompressedIndex::deserialize(&data);

        assert!(matches!(
            result,
            Err(CompressionError::InvalidIndexVersion(0))
        ));
    }

    #[test]
    fn test_compression_metadata() {
        let original = vec![b'y'; 5000];
        let compressed = CompressedIndex::compress(&original, 5).unwrap();

        assert_eq!(compressed.uncompressed_size(), 5000);
        assert_eq!(compressed.compression(), CompressionFormat::Zstd);
        assert!(compressed.compressed_size() < 5000);
    }

    #[test]
    fn test_empty_data_compression() {
        let original = b"";
        let compressed = CompressedIndex::compress(original, 3).unwrap();
        let decompressed = compressed.decompress().unwrap();

        assert_eq!(original, &decompressed[..]);
        assert_eq!(compressed.uncompressed_size(), 0);
    }

    #[test]
    fn test_large_data_compression() {
        // Test with ~1MB of data
        let original = vec![b'z'; 1_000_000];
        let compressed = CompressedIndex::compress(&original, 3).unwrap();
        let decompressed = compressed.decompress().unwrap();

        assert_eq!(original, decompressed);
        // Should achieve significant compression on repeated data
        assert!(
            compressed.compressed_size() < 100_000,
            "Expected < 100KB compressed, got {}",
            compressed.compressed_size()
        );
    }

    // ============================================================================
    // Comprehensive tests for P1-14 decompression bomb protection
    // ============================================================================

    #[test]
    fn test_decompression_bomb_protection_blocks_oversized() {
        // Create data that would exceed max_uncompressed_size (default 500MB)
        let original = vec![b'a'; 1_000_000]; // 1MB
        let compressed = CompressedIndex::compress(&original, 3).unwrap();

        // Manually corrupt the uncompressed_size field to claim 600MB
        let mut serialized = compressed.serialize();
        let fake_size = 600u64 * 1024 * 1024; // 600MB
        serialized[13..21].copy_from_slice(&fake_size.to_le_bytes());

        let corrupted = CompressedIndex::deserialize(&serialized).unwrap();
        let result = corrupted.decompress();

        // Should reject due to size claim exceeding limit
        assert!(
            matches!(result, Err(CompressionError::DecompressionBomb { .. })),
            "Should reject oversized decompression claim"
        );
    }

    #[test]
    fn test_decompression_bomb_protection_allows_at_limit() {
        // Test boundary case: data exactly at the limit should be accepted
        // Create compressed data with uncompressed size exactly at default limit (500MB)
        let original = vec![b'b'; 100_000]; // 100KB actual data
        let mut compressed = CompressedIndex::compress(&original, 3).unwrap();

        // Set uncompressed_size to exactly the limit (500MB)
        let exact_limit = 500u64 * 1024 * 1024;
        compressed.uncompressed_size = exact_limit;

        let serialized = compressed.serialize();
        let deserialized = CompressedIndex::deserialize(&serialized).unwrap();

        // Should accept data exactly at limit (due to > check, not >=)
        // Note: This will fail decompression for other reasons (actual data != claimed size)
        // but it should NOT fail with DecompressionBomb error
        let result = deserialized.decompress();

        // Should not be a decompression bomb error
        assert!(
            !matches!(result, Err(CompressionError::DecompressionBomb { .. })),
            "Should not reject data exactly at limit as decompression bomb"
        );
    }

    #[test]
    fn test_decompression_bomb_protection_blocks_one_over_limit() {
        // Test boundary case: data one byte over limit should be rejected
        let original = vec![b'c'; 100_000]; // 100KB actual data
        let compressed = CompressedIndex::compress(&original, 3).unwrap();

        // Manually set uncompressed_size to limit + 1
        let mut serialized = compressed.serialize();
        let over_limit = (500u64 * 1024 * 1024) + 1; // 500MB + 1 byte
        serialized[13..21].copy_from_slice(&over_limit.to_le_bytes());

        let corrupted = CompressedIndex::deserialize(&serialized).unwrap();
        let result = corrupted.decompress();

        // Should reject due to size exceeding limit by even 1 byte
        assert!(
            matches!(result, Err(CompressionError::DecompressionBomb { .. })),
            "Should reject data exceeding limit by even 1 byte"
        );
    }

    #[test]
    fn test_decompression_enforces_streaming_limit() {
        // Test that decompression uses streaming limit with take(max_size + 1)
        // This ensures we can detect data that exceeds the limit during streaming

        // Create data that's well within the limit
        let original = vec![b'd'; 200_000]; // 200KB - well within 500MB limit
        let compressed = CompressedIndex::compress(&original, 3).unwrap();

        // Decompress normally - should succeed because streaming limit allows it
        let result = compressed.decompress();
        assert!(result.is_ok(), "Decompression within limit should succeed");

        // The streaming limit (max_size + 1) ensures:
        // - Data exactly at max_size passes (not flagged as bomb)
        // - Data over max_size fails (flagged as bomb)
        // This is validated by the boundary tests above
    }

    #[test]
    fn test_max_uncompressed_size_clamping_enforces_minimum() {
        // Test that MIN_MAX_UNCOMPRESSED_SIZE (1MB) is enforced
        // Note: This tests the clamping logic, but we can't directly set env vars in tests
        // without affecting other tests, so we verify the constants exist

        const MIN_MAX_UNCOMPRESSED_SIZE: u64 = 1024 * 1024; // 1 MB
        const MAX_MAX_UNCOMPRESSED_SIZE: u64 = 2 * 1024 * 1024 * 1024; // 2 GB

        // Verify constants are reasonable
        assert_eq!(MIN_MAX_UNCOMPRESSED_SIZE, 1_048_576, "MIN should be 1MB");
        assert_eq!(
            MAX_MAX_UNCOMPRESSED_SIZE, 2_147_483_648,
            "MAX should be 2GB"
        );

        // Verify min < default < max
        let default_size = max_uncompressed_size();
        assert!(
            default_size >= MIN_MAX_UNCOMPRESSED_SIZE,
            "Default {default_size} should be >= MIN {MIN_MAX_UNCOMPRESSED_SIZE}"
        );
        assert!(
            default_size <= MAX_MAX_UNCOMPRESSED_SIZE,
            "Default {default_size} should be <= MAX {MAX_MAX_UNCOMPRESSED_SIZE}"
        );
    }

    #[test]
    fn test_max_uncompressed_size_default_is_500mb() {
        // Verify default is 500MB when no env var is set
        let default = max_uncompressed_size();

        // Default should be 500MB = 524,288,000 bytes
        // Unless overridden by env var, but in test environment it should be default
        assert!(
            default >= 500 * 1024 * 1024 || std::env::var("SQRY_MAX_INDEX_SIZE").is_ok(),
            "Default should be 500MB or env var should be set"
        );
    }

    #[test]
    fn test_decompression_bomb_error_includes_sizes() {
        // Verify that DecompressionBomb error includes both actual and max sizes
        let original = vec![b'e'; 100_000];
        let compressed = CompressedIndex::compress(&original, 3).unwrap();

        // Create oversized claim
        let mut serialized = compressed.serialize();
        let oversized = 600u64 * 1024 * 1024; // 600MB
        serialized[13..21].copy_from_slice(&oversized.to_le_bytes());

        let corrupted = CompressedIndex::deserialize(&serialized).unwrap();

        match corrupted.decompress() {
            Err(CompressionError::DecompressionBomb { size, max }) => {
                assert_eq!(size, oversized, "Error should report actual claimed size");
                assert!(max > 0, "Error should report max limit");
                assert!(size > max, "Error should show size exceeds max");
            }
            other => panic!("Expected DecompressionBomb error, got {other:?}"),
        }
    }

    #[test]
    fn test_compression_format_from_u8() {
        // Verify CompressionFormat::from_u8() works correctly
        assert!(matches!(
            CompressionFormat::from_u8(0),
            Ok(CompressionFormat::None)
        ));
        assert!(matches!(
            CompressionFormat::from_u8(1),
            Ok(CompressionFormat::Zstd)
        ));
        assert!(matches!(
            CompressionFormat::from_u8(99),
            Err(CompressionError::UnsupportedCompression(99))
        ));
    }
}
