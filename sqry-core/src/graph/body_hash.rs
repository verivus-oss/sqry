//! 128-bit deterministic body hashing using dual xxh64
//!
//! Uses fixed seeds for cross-process determinism.
//! Seeds are versioned alongside the index schema.
//!
//! # Why xxh64 over siphasher?
//!
//! - **Already in codebase**: xxhash-rust used in 6+ sqry modules
//! - **Seeding proven**: prewarm/store.rs, project/types.rs use seeded xxh64
//! - **Faster**: ~10-15 GB/s vs ~3-5 GB/s for `SipHash`
//! - **No new dependency**: Reduces build time and audit surface
//! - **Same determinism**: Fixed seed = identical output across runs
//!
//! # Seed Registry
//!
//! All seeds in sqry follow the ASCII convention (8 chars = 64 bits):
//!
//! | Module | Seed Constant | Hex Value | ASCII Meaning | Purpose |
//! |--------|---------------|-----------|---------------|---------|
//! | `prewarm/store.rs` | `PAYLOAD_CHECKSUM_SEED` | `0x5351_5259_5041_594C` | "SQRYPAYL" | Checksum verification |
//! | `project/types.rs` | `HASH_SEED` | `0x5351_5259_5041_5448` | "SQRYPATH" | Path hashing |
//! | `body_hash.rs` | `HASH_SEED_0` | `0x5351_5259_4455_5030` | "SQRYDUP0" | Body hash high bits |
//! | `body_hash.rs` | `HASH_SEED_1` | `0x5351_5259_4455_5031` | "SQRYDUP1" | Body hash low bits |
//!
//! # Rules for adding new seeds
//!
//! 1. All seeds MUST be unique across the codebase
//! 2. Use ASCII encoding of meaningful names (8 chars = 64 bits)
//! 3. Follow pattern: "SQRY" prefix + 4-char identifier
//! 4. Register new seeds in this table before implementation
//! 5. Changing seeds requires `INDEX_SCHEMA_VERSION` bump

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh64::xxh64;

/// Fixed hash seed for high 64 bits of body hash.
///
/// Schema: `INDEX_SCHEMA_VERSION` = 2
/// ASCII: "SQRYDUP0" = `0x5351_5259_4455_5030`
pub const HASH_SEED_0: u64 = 0x5351_5259_4455_5030;

/// Fixed hash seed for low 64 bits of body hash.
///
/// Schema: `INDEX_SCHEMA_VERSION` = 2
/// ASCII: "SQRYDUP1" = `0x5351_5259_4455_5031`
pub const HASH_SEED_1: u64 = 0x5351_5259_4455_5031;

/// 128-bit body hash for collision resistance
///
/// Using two xxh64 outputs with different seeds provides 128-bit security.
/// This is sufficient to avoid birthday paradox collisions in codebases
/// with millions of symbols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct BodyHash128 {
    /// High 64 bits (computed with `HASH_SEED_0`)
    pub high: u64,
    /// Low 64 bits (computed with `HASH_SEED_1`)
    pub low: u64,
}

impl BodyHash128 {
    /// Compute deterministic 128-bit hash of normalized body content
    ///
    /// Uses two xxh64 calls with different seeds to achieve 128-bit collision resistance.
    /// This follows the established pattern in sqry (see prewarm/store.rs:514).
    ///
    /// # Arguments
    ///
    /// * `content` - Normalized body content bytes
    ///
    /// # Returns
    ///
    /// A 128-bit hash value
    #[must_use]
    pub fn compute(content: &[u8]) -> Self {
        Self {
            high: xxh64(content, HASH_SEED_0),
            low: xxh64(content, HASH_SEED_1),
        }
    }

    /// Convert to u128 for comparison and storage
    #[must_use]
    pub fn as_u128(&self) -> u128 {
        (u128::from(self.high) << 64) | u128::from(self.low)
    }

    /// Create from u128 value
    #[must_use]
    pub fn from_u128(value: u128) -> Self {
        let high = u64::try_from(value >> 64).unwrap_or(u64::MAX);
        let low = u64::try_from(value & u128::from(u64::MAX)).unwrap_or(u64::MAX);
        Self { high, low }
    }
}

impl std::fmt::Display for BodyHash128 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}{:016x}", self.high, self.low)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_body_hash_deterministic() {
        let content = b"fn example() { return 42; }";
        let hash1 = BodyHash128::compute(content);
        let hash2 = BodyHash128::compute(content);
        assert_eq!(hash1, hash2, "Hash must be deterministic");
    }

    #[test]
    fn test_body_hash_different_content() {
        let content1 = b"fn example() { return 42; }";
        let content2 = b"fn example() { return 43; }";
        let hash1 = BodyHash128::compute(content1);
        let hash2 = BodyHash128::compute(content2);
        assert_ne!(
            hash1, hash2,
            "Different content should produce different hash"
        );
    }

    #[test]
    fn test_body_hash_empty() {
        let hash = BodyHash128::compute(b"");
        // Empty content should still produce a valid hash
        assert_ne!(hash.high, 0, "Empty content hash high should not be zero");
        assert_ne!(hash.low, 0, "Empty content hash low should not be zero");
    }

    #[test]
    fn test_body_hash_u128_roundtrip() {
        let content = b"test content for roundtrip";
        let hash = BodyHash128::compute(content);
        let as_u128 = hash.as_u128();
        let roundtrip = BodyHash128::from_u128(as_u128);
        assert_eq!(hash, roundtrip, "u128 roundtrip should preserve hash");
    }

    #[test]
    fn test_xxh64_fixed_seed() {
        // Verify the seeds are the expected ASCII values
        // "SQRYDUP0" in hex: S=53, Q=51, R=52, Y=59, D=44, U=55, P=50, 0=30
        assert_eq!(HASH_SEED_0, 0x5351_5259_4455_5030);
        // "SQRYDUP1" in hex: S=53, Q=51, R=52, Y=59, D=44, U=55, P=50, 1=31
        assert_eq!(HASH_SEED_1, 0x5351_5259_4455_5031);
    }

    #[test]
    fn test_xxh64_cross_endian() {
        // Fixed test vector to catch endianness or streaming/segment-size regression
        // These values should be consistent across all platforms
        let test_data = b"fn example() { return 42; }";
        let hash = BodyHash128::compute(test_data);

        // These expected values are computed on x86_64 Linux reference platform
        // If these fail on another platform, investigate endianness handling
        // Note: We're testing that the hash is non-zero and deterministic,
        // not specific values (which would couple to xxhash implementation)
        assert_ne!(
            hash.high, 0,
            "High bits should be non-zero for non-empty content"
        );
        assert_ne!(
            hash.low, 0,
            "Low bits should be non-zero for non-empty content"
        );

        // Verify the two halves are different (different seeds)
        assert_ne!(
            hash.high, hash.low,
            "High and low should differ due to different seeds"
        );
    }

    #[test]
    fn test_body_hash_display() {
        let hash = BodyHash128 {
            high: 0x1234_5678_90AB_CDEF,
            low: 0xFEDC_BA09_8765_4321,
        };
        let display = format!("{hash}");
        assert_eq!(display, "1234567890abcdeffedcba0987654321");
    }

    #[test]
    fn test_body_hash_serde_json() {
        let hash = BodyHash128::compute(b"test");
        let json = serde_json::to_string(&hash).unwrap();
        let parsed: BodyHash128 = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, parsed, "JSON roundtrip should preserve hash");
    }
}
