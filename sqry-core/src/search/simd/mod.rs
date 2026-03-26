//! SIMD-accelerated text search operations
//!
//! This module provides SIMD-optimized implementations of core search operations:
//! - Substring search (Boyer-Moore-Horspool with SIMD)
//! - Trigram extraction (bulk loading with SIMD)
//! - ASCII case conversion (range check with SIMD)
//!
//! Platform support:
//! - x86_64: AVX2 (primary), SSE4.2 (fallback)
//! - aarch64: NEON
//! - Other: Scalar fallback
//!
//! Safety:
//! - All SIMD operations use safe wrappers from std::arch
//! - Runtime feature detection ensures CPU support
//! - Fallback to scalar when SIMD unavailable
//! - Property tests validate SIMD ≡ scalar

pub mod scalar;

mod common;

#[cfg(target_arch = "x86_64")]
pub mod avx2;

#[cfg(target_arch = "x86_64")]
pub mod sse42;

#[cfg(target_arch = "aarch64")]
pub mod neon;

use std::fmt;

/// Search result: byte offset of match, or None
pub type SearchResult = Option<usize>;

/// SIMD platform in use
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimdPlatform {
    /// `x86_64` AVX2 (32-byte vectors)
    Avx2,
    /// `x86_64` SSE4.2 (16-byte vectors)
    Sse42,
    /// ARM64 NEON (16-byte vectors)
    Neon,
    /// Scalar fallback (no SIMD)
    Scalar,
}

impl fmt::Display for SimdPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimdPlatform::Avx2 => write!(f, "AVX2"),
            SimdPlatform::Sse42 => write!(f, "SSE4.2"),
            SimdPlatform::Neon => write!(f, "NEON"),
            SimdPlatform::Scalar => write!(f, "Scalar"),
        }
    }
}

/// Detect the best available SIMD platform for the current CPU
#[must_use]
pub fn detect_platform() -> SimdPlatform {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            log::debug!("SIMD platform: AVX2");
            return SimdPlatform::Avx2;
        }
        if is_x86_feature_detected!("sse4.2") {
            log::debug!("SIMD platform: SSE4.2");
            return SimdPlatform::Sse42;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            log::debug!("SIMD platform: NEON");
            return SimdPlatform::Neon;
        }
    }

    log::debug!("SIMD platform: Scalar (no SIMD available)");
    SimdPlatform::Scalar
}

/// Search for needle in haystack using the best available SIMD implementation
///
/// # Safety
/// This function performs runtime feature detection and dispatches to the
/// appropriate SIMD implementation. All SIMD code uses safe wrappers from
/// `std::arch`, so this function is safe to call.
///
/// # Examples
/// ```
/// use sqry_core::search::simd::search;
///
/// let haystack = b"hello world";
/// let needle = b"world";
/// assert_eq!(search(haystack, needle), Some(6));
/// ```
#[must_use]
pub fn search(haystack: &[u8], needle: &[u8]) -> SearchResult {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }

    // Phase 2: AVX2 implementation active
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::search(haystack, needle) };
        }
        // Phase 3: SSE4.2 fallback for x86_64
        if is_x86_feature_detected!("sse4.2") {
            return unsafe { sse42::search(haystack, needle) };
        }
    }

    // Phase 3: NEON support for aarch64
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return unsafe { neon::search(haystack, needle) };
        }
    }

    scalar::search(haystack, needle)
}

/// Extract trigrams from text using the best available SIMD implementation
///
/// A trigram is a 3-character sliding window over the input text.
/// For example: `"abcd"` → `["abc", "bcd"]`
///
/// Strings shorter than 3 characters return a single-element vector with
/// the original string.
///
/// # Examples
/// ```
/// use sqry_core::search::simd::extract_trigrams;
///
/// let trigrams = extract_trigrams("hello");
/// assert_eq!(trigrams, vec!["hel", "ell", "llo"]);
/// ```
#[must_use]
pub fn extract_trigrams(text: &str) -> Vec<String> {
    if text.len() < 3 {
        return vec![text.to_string()];
    }

    // Phase 2: AVX2 implementation active
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::extract_trigrams(text) };
        }
        // Phase 3: SSE4.2 fallback for x86_64
        if is_x86_feature_detected!("sse4.2") {
            return unsafe { sse42::extract_trigrams(text) };
        }
    }

    // Phase 3: NEON support for aarch64
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return unsafe { neon::extract_trigrams(text) };
        }
    }

    scalar::extract_trigrams(text)
}

/// Convert ASCII text to lowercase using the best available SIMD implementation
///
/// Only ASCII characters (A-Z) are converted to lowercase. Non-ASCII characters
/// are preserved unchanged.
///
/// # Examples
/// ```
/// use sqry_core::search::simd::to_lowercase_ascii;
///
/// assert_eq!(to_lowercase_ascii("HELLO"), "hello");
/// assert_eq!(to_lowercase_ascii("HeLLo"), "hello");
/// ```
#[must_use]
pub fn to_lowercase_ascii(text: &str) -> String {
    // Phase 2: AVX2 implementation active
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { avx2::to_lowercase_ascii(text) };
        }
        // Phase 3: SSE4.2 fallback for x86_64
        if is_x86_feature_detected!("sse4.2") {
            return unsafe { sse42::to_lowercase_ascii(text) };
        }
    }

    // Phase 3: NEON support for aarch64
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return unsafe { neon::to_lowercase_ascii(text) };
        }
    }

    scalar::to_lowercase_ascii(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_platform() {
        let platform = detect_platform();
        // Should detect some platform (even if just scalar)
        assert!(matches!(
            platform,
            SimdPlatform::Avx2 | SimdPlatform::Sse42 | SimdPlatform::Neon | SimdPlatform::Scalar
        ));
    }

    #[test]
    fn test_platform_display() {
        assert_eq!(SimdPlatform::Avx2.to_string(), "AVX2");
        assert_eq!(SimdPlatform::Sse42.to_string(), "SSE4.2");
        assert_eq!(SimdPlatform::Neon.to_string(), "NEON");
        assert_eq!(SimdPlatform::Scalar.to_string(), "Scalar");
    }

    #[test]
    fn test_search_empty_needle() {
        let haystack = b"hello";
        let needle = b"";
        assert_eq!(search(haystack, needle), Some(0));
    }

    #[test]
    fn test_search_needle_too_long() {
        let haystack = b"hi";
        let needle = b"hello";
        assert_eq!(search(haystack, needle), None);
    }

    #[test]
    fn test_extract_trigrams_short_string() {
        assert_eq!(extract_trigrams("ab"), vec!["ab"]);
        assert_eq!(extract_trigrams(""), vec![""]);
    }

    #[test]
    fn test_to_lowercase_ascii_empty() {
        assert_eq!(to_lowercase_ascii(""), "");
    }

    #[test]
    fn test_extract_trigrams_ascii_matches_scalar() {
        let inputs = [
            "hello",
            "abc",
            "abcdefghijklmnopqrstuvwxyz0123456789",
            "createCompilerHost",
            "aaaa",
            "HELLO_WORLD",
        ];
        for input in &inputs {
            let mut dispatched = extract_trigrams(input);
            let mut scalar_result = scalar::extract_trigrams(input);
            dispatched.sort();
            scalar_result.sort();
            assert_eq!(
                dispatched, scalar_result,
                "SIMD ≡ scalar mismatch for ASCII input: {input}"
            );
        }
    }

    #[test]
    fn test_extract_trigrams_non_ascii_matches_scalar() {
        let inputs = ["héllo", "日本語", "café", "naïve", "über"];
        for input in &inputs {
            let mut dispatched = extract_trigrams(input);
            let mut scalar_result = scalar::extract_trigrams(input);
            dispatched.sort();
            scalar_result.sort();
            assert_eq!(
                dispatched, scalar_result,
                "SIMD ≡ scalar mismatch for non-ASCII input: {input}"
            );
        }
    }
}
