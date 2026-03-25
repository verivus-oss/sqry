//! BLAKE3 hashing utilities for cache module.
//!
//! This module provides fast, cryptographic hashing functions for cache key generation
//! and content verification. BLAKE3 is significantly faster than SHA-256 (~1 GB/s)
//! while maintaining cryptographic security properties.
//!
//! # Usage
//!
//! ```no_run
//! use sqry_core::hash::{hash_file, hash_bytes, Blake3Hash};
//! use std::path::Path;
//!
//! // Hash file contents
//! let file_hash = hash_file(Path::new("example.rs"))?;
//!
//! // Hash byte slice
//! let content_hash = hash_bytes(b"hello world");
//!
//! // Use in cache keys
//! println!("File hash: {}", file_hash);
//! # Ok::<(), std::io::Error>(())
//! ```

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::config::buffers::parse_buffer_size;

/// BLAKE3 hash output (32 bytes / 256 bits).
///
/// This type alias provides semantic clarity when working with hash values
/// in cache keys and headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Blake3Hash([u8; 32]);

impl Blake3Hash {
    /// Create a hash from a 32-byte array.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::hash::Blake3Hash;
    ///
    /// let bytes = [0u8; 32];
    /// let hash = Blake3Hash::from_bytes(bytes);
    /// ```
    #[inline]
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Get the hash as a byte slice.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::hash::Blake3Hash;
    ///
    /// let hash = Blake3Hash::from_bytes([0u8; 32]);
    /// let bytes: &[u8] = hash.as_bytes();
    /// assert_eq!(bytes.len(), 32);
    /// ```
    #[inline]
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Convert the hash to a hex string.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::hash::Blake3Hash;
    ///
    /// let hash = Blake3Hash::from_bytes([0u8; 32]);
    /// let hex = hash.to_hex();
    /// assert_eq!(hex.len(), 64); // 32 bytes * 2 hex digits
    /// ```
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a hash from a hex string.
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not valid hex or not 64 characters long.
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::hash::Blake3Hash;
    ///
    /// let hex = "0000000000000000000000000000000000000000000000000000000000000000";
    /// let hash = Blake3Hash::from_hex(hex)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_hex(hex_str: &str) -> Result<Self, hex::FromHexError> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(hex_str, &mut bytes)?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl serde::Serialize for Blake3Hash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            serializer.serialize_str(&self.to_hex())
        } else {
            // For binary formats, serialize as a fixed-size array
            self.0.serialize(serializer)
        }
    }
}

impl<'de> serde::Deserialize<'de> for Blake3Hash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            let hex_str = String::deserialize(deserializer)?;
            Self::from_hex(&hex_str).map_err(serde::de::Error::custom)
        } else {
            // For binary formats, deserialize as a fixed-size array
            let bytes = <[u8; 32]>::deserialize(deserializer)?;
            Ok(Self(bytes))
        }
    }
}

/// Hash the contents of a file using BLAKE3.
///
/// This function reads the file in chunks to avoid loading large files
/// entirely into memory.
///
/// # Errors
///
/// Returns an I/O error if the file cannot be read.
///
/// # Examples
///
/// ```no_run
/// use sqry_core::hash::hash_file;
/// use std::path::Path;
///
/// let hash = hash_file(Path::new("example.rs"))?;
/// println!("File hash: {}", hash);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn hash_file(path: &Path) -> io::Result<Blake3Hash> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();

    // Read in chunks for efficiency (respects SQRY_PARSE_BUFFER env var)
    let mut buffer = vec![0u8; parse_buffer_size()];
    loop {
        let bytes_read = file.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let hash = hasher.finalize();
    Ok(Blake3Hash::from_bytes(*hash.as_bytes()))
}

/// Hash a byte slice using BLAKE3.
///
/// This is a convenience function for hashing in-memory data.
///
/// # Examples
///
/// ```
/// use sqry_core::hash::hash_bytes;
///
/// let hash = hash_bytes(b"hello world");
/// println!("Content hash: {}", hash);
/// ```
#[inline]
#[must_use]
pub fn hash_bytes(content: &[u8]) -> Blake3Hash {
    let hash = blake3::hash(content);
    Blake3Hash::from_bytes(*hash.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_hash_bytes_deterministic() {
        let content = b"hello world";
        let hash1 = hash_bytes(content);
        let hash2 = hash_bytes(content);

        assert_eq!(
            hash1, hash2,
            "Hashing same content should produce same hash"
        );
    }

    #[test]
    fn test_hash_bytes_different_content() {
        let hash1 = hash_bytes(b"hello world");
        let hash2 = hash_bytes(b"hello sqry");

        assert_ne!(
            hash1, hash2,
            "Different content should produce different hashes"
        );
    }

    #[test]
    fn test_hash_empty_content() {
        let hash = hash_bytes(b"");

        // BLAKE3 hash of empty string (known value)
        let expected_hex = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";
        assert_eq!(hash.to_hex(), expected_hex);
    }

    #[test]
    fn test_hash_file() -> io::Result<()> {
        // Create temporary file with known content
        let mut temp_file = NamedTempFile::new()?;
        temp_file.write_all(b"test content for hashing")?;
        temp_file.flush()?;

        let hash = hash_file(temp_file.path())?;

        // Hash should match hash_bytes of same content
        let expected = hash_bytes(b"test content for hashing");
        assert_eq!(hash, expected);

        Ok(())
    }

    #[test]
    fn test_hash_file_large() -> io::Result<()> {
        // Test with file larger than buffer size (65KB)
        let mut temp_file = NamedTempFile::new()?;
        let large_content = vec![b'x'; 100_000]; // 100KB
        temp_file.write_all(&large_content)?;
        temp_file.flush()?;

        let hash1 = hash_file(temp_file.path())?;
        let hash2 = hash_bytes(&large_content);

        assert_eq!(hash1, hash2, "Large file hash should match bytes hash");

        Ok(())
    }

    #[test]
    fn test_hash_file_nonexistent() {
        let result = hash_file(Path::new("/nonexistent/file.txt"));

        assert!(
            result.is_err(),
            "Hashing nonexistent file should return error"
        );
    }

    #[test]
    fn test_blake3hash_hex_roundtrip() {
        let original = hash_bytes(b"test");
        let hex = original.to_hex();
        let parsed = Blake3Hash::from_hex(&hex).unwrap();

        assert_eq!(original, parsed, "Hex encoding/decoding should roundtrip");
    }

    #[test]
    fn test_blake3hash_display() {
        let hash = hash_bytes(b"test");
        let display = format!("{hash}");
        let to_hex = hash.to_hex();

        assert_eq!(display, to_hex, "Display should match to_hex()");
    }

    #[test]
    fn test_blake3hash_from_hex_invalid() {
        // Too short
        assert!(Blake3Hash::from_hex("abc").is_err());

        // Invalid hex
        assert!(
            Blake3Hash::from_hex(
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
            )
            .is_err()
        );

        // Wrong length (62 chars, not 64)
        assert!(
            Blake3Hash::from_hex("00000000000000000000000000000000000000000000000000000000000000")
                .is_err()
        );
    }

    #[test]
    fn test_blake3hash_serde_json() {
        let hash = hash_bytes(b"test");

        // Serialize to JSON (human-readable)
        let json = serde_json::to_string(&hash).unwrap();
        assert!(json.contains(&hash.to_hex()));

        // Deserialize from JSON
        let parsed: Blake3Hash = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, parsed);
    }

    #[test]
    fn test_blake3hash_serde_postcard() {
        let hash = hash_bytes(b"test");

        // Serialize to postcard (binary)
        let binary = postcard::to_allocvec(&hash).unwrap();
        // Postcard adds overhead for the struct wrapper, so it's more than 32 bytes
        // The important part is that deserialization works correctly
        assert!(
            binary.len() >= 32,
            "Postcard should serialize at least the 32 hash bytes"
        );

        // Deserialize from postcard
        let parsed: Blake3Hash = postcard::from_bytes(&binary).unwrap();
        assert_eq!(hash, parsed, "Roundtrip serialization should preserve hash");
    }

    #[test]
    fn test_known_blake3_vectors() {
        // Test against known BLAKE3 test vectors
        // Source: https://github.com/BLAKE3-team/BLAKE3/blob/master/test_vectors/test_vectors.json

        // Empty string
        let hash = hash_bytes(b"");
        assert_eq!(
            hash.to_hex(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );

        // Single byte
        let hash = hash_bytes(&[0]);
        assert_eq!(
            hash.to_hex(),
            "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213"
        );
    }
}
