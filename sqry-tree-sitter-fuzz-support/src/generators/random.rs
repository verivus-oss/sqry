//! Seeded random byte generator.
//!
//! Generates pseudo-random bytes using a seeded PRNG for reproducibility.
//! This allows tests to fail deterministically and be debugged locally.

use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};

/// Default seed for reproducible random generation.
///
/// This seed is documented and shared across all tests for consistency.
pub const DEFAULT_SEED: u64 = 0x5152_595F_5445_5354; // "SQRY_TEST" in hex

/// Generates random bytes using a seeded PRNG.
///
/// # Parameters
/// - `size`: Number of bytes to generate
/// - `seed`: Seed for the PRNG (use `DEFAULT_SEED` for standard tests)
///
/// # Returns
/// `Vec<u8>` of `size` random bytes.
///
/// # Reproducibility
/// Using the same seed will always produce the same output, allowing
/// deterministic test failures to be reproduced locally.
///
/// # Examples
/// ```
/// use sqry_tree_sitter_fuzz_support::generators::random::{generate_random_bytes, DEFAULT_SEED};
///
/// let random1 = generate_random_bytes(100, DEFAULT_SEED);
/// let random2 = generate_random_bytes(100, DEFAULT_SEED);
/// assert_eq!(random1, random2); // Deterministic!
/// ```
#[must_use]
pub fn generate_random_bytes(size: usize, seed: u64) -> Vec<u8> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut result = vec![0u8; size];
    rng.fill_bytes(&mut result);
    result
}

/// Generates random bytes using the default seed.
///
/// Convenience wrapper for `generate_random_bytes(size, DEFAULT_SEED)`.
#[must_use]
pub fn generate_random_bytes_default(size: usize) -> Vec<u8> {
    generate_random_bytes(size, DEFAULT_SEED)
}

/// Pre-defined seeds for different test scenarios.
pub mod seeds {
    /// Default seed for standard malformed input tests.
    pub const STANDARD: u64 = super::DEFAULT_SEED;

    /// Seed for symbol extraction tests.
    pub const SYMBOLS: u64 = 0x0053_594D_424F_4C53; // "SYMBOLS" in hex

    /// Seed for relations tests.
    pub const RELATIONS: u64 = 0x0052_454C_4154_494F; // "RELATIO" in hex (7 chars)

    /// Seed for parser stress tests.
    pub const STRESS: u64 = 0x0053_5452_4553_535F; // "STRESS_" in hex
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_random_bytes() {
        let result = generate_random_bytes(100, DEFAULT_SEED);
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_deterministic() {
        let seed = 12345u64;
        let result1 = generate_random_bytes(1000, seed);
        let result2 = generate_random_bytes(1000, seed);

        assert_eq!(result1, result2, "Same seed should produce same output");
    }

    #[test]
    fn test_different_seeds_produce_different_output() {
        let result1 = generate_random_bytes(1000, 1);
        let result2 = generate_random_bytes(1000, 2);

        assert_ne!(
            result1, result2,
            "Different seeds should produce different output"
        );
    }

    #[test]
    fn test_default_seed_wrapper() {
        let result1 = generate_random_bytes_default(100);
        let result2 = generate_random_bytes(100, DEFAULT_SEED);

        assert_eq!(result1, result2);
    }

    #[test]
    fn test_seed_constants() {
        assert_eq!(seeds::STANDARD, DEFAULT_SEED);
        assert_ne!(seeds::SYMBOLS, seeds::RELATIONS);
        assert_ne!(seeds::STRESS, seeds::SYMBOLS);
    }

    #[test]
    fn test_empty_size() {
        let result = generate_random_bytes(0, DEFAULT_SEED);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_large_size() {
        let result = generate_random_bytes(10_000, DEFAULT_SEED);
        assert_eq!(result.len(), 10_000);

        // Should have variety (not all zeros)
        let unique_bytes: std::collections::HashSet<u8> = result.iter().copied().collect();
        assert!(
            unique_bytes.len() > 100,
            "Should have variety of byte values"
        );
    }

    #[test]
    fn test_reproducibility_across_calls() {
        // Call multiple times to ensure reproducibility
        let calls: Vec<_> = (0..5)
            .map(|_| generate_random_bytes(50, seeds::STANDARD))
            .collect();

        // All calls should produce identical output
        for i in 1..calls.len() {
            assert_eq!(calls[0], calls[i]);
        }
    }
}
