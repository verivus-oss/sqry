#![allow(
    unsafe_op_in_unsafe_fn,
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr,
    clippy::must_use_candidate
)]

//! AVX2-accelerated search operations (`x86_64` only)
//!
//! This module provides AVX2 (32-byte vector) implementations of:
//! - Substring search using `_mm256_cmpeq_epi8`
//! - Trigram extraction using `_mm256_loadu_si256`
//! - ASCII lowercase using `_mm256_cmpgt_epi8`
//!
//! All functions require AVX2 support (runtime detection in mod.rs).
//! Falls back to scalar implementation if AVX2 unavailable.

use super::SearchResult;
use super::common::{
    advance_step, build_skip_table, early_search_result, find_match_in_mask, tail_match,
};
use std::arch::x86_64::{
    __m256i, _mm256_add_epi8, _mm256_and_si256, _mm256_cmpeq_epi8, _mm256_cmpgt_epi8,
    _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_set1_epi8, _mm256_storeu_si256,
    _mm256_sub_epi8,
};
use std::collections::HashSet;

const LANE_BYTES: usize = 32;

/// AVX2 substring search using Boyer-Moore-Horspool with SIMD first-byte scan
///
/// # Algorithm
/// 1. Build BMH skip table for efficient skipping
/// 2. Use AVX2 to scan 32 bytes at once for first-byte matches
/// 3. Verify candidates with SIMD memcmp
/// 4. Skip ahead using BMH table on mismatch
///
/// # Safety
/// Caller must ensure AVX2 is available (checked by runtime detection in mod.rs)
///
/// # Performance
/// Expected 3-4x speedup vs scalar for needles >8 bytes
#[target_feature(enable = "avx2")]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr,
    clippy::must_use_candidate
)]
#[must_use]
pub unsafe fn search(haystack: &[u8], needle: &[u8]) -> SearchResult {
    if let Some(result) = early_search_result(haystack, needle, search_single_byte_avx2) {
        return result;
    }

    // Build skip table for Boyer-Moore-Horspool
    let skip_table = build_skip_table(needle);
    let needle_len = needle.len();

    // Broadcast first byte of needle to all lanes
    let first_vec = _mm256_set1_epi8(needle[0] as i8);

    let mut pos = 0;
    while pos <= haystack.len().saturating_sub(needle_len) {
        let at_tail = pos + LANE_BYTES > haystack.len();
        if let Some(candidate_pos) =
            scan_window_avx2(haystack, needle, pos, needle_len, first_vec, at_tail)
        {
            return Some(candidate_pos);
        }

        // BMH skip: advance by skip distance based on last character in window
        pos += advance_step(haystack, needle_len, pos, &skip_table, at_tail);
    }

    None
}

#[target_feature(enable = "avx2")]
unsafe fn scan_window_avx2(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    needle_len: usize,
    first_vec: __m256i,
    at_tail: bool,
) -> Option<usize> {
    if at_tail {
        return tail_match(haystack, needle, pos, needle_len);
    }

    scan_candidates_avx2(haystack, needle, pos, needle_len, first_vec)
}

#[target_feature(enable = "avx2")]
unsafe fn scan_candidates_avx2(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    needle_len: usize,
    first_vec: __m256i,
) -> Option<usize> {
    // Load 32 bytes from haystack at position pos
    let hay_vec = _mm256_loadu_si256(haystack[pos..].as_ptr() as *const __m256i);

    // Compare first byte: find all positions where first byte matches
    let first_cmp = _mm256_cmpeq_epi8(hay_vec, first_vec);
    let first_mask = _mm256_movemask_epi8(first_cmp);

    if first_mask == 0 {
        return None;
    }

    #[allow(
        clippy::cast_sign_loss,
        reason = "movemask returns a signed bitmask; casting preserves raw bits"
    )]
    let first_mask_u32 = first_mask as u32;

    find_match_in_mask(
        haystack,
        needle,
        pos,
        needle_len,
        first_mask_u32,
        LANE_BYTES,
        verify_match_avx2,
    )
}

/// Verify full needle match at candidate position using AVX2
///
/// # Safety
/// Requires AVX2 support, caller must ensure it's available
#[target_feature(enable = "avx2")]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr
)]
unsafe fn verify_match_avx2(haystack: &[u8], needle: &[u8], pos: usize) -> bool {
    let mut offset = 0;

    // Process 32 bytes at a time
    while offset + 32 <= needle.len() {
        let hay_vec = _mm256_loadu_si256(haystack[pos + offset..].as_ptr() as *const __m256i);
        let needle_vec = _mm256_loadu_si256(needle[offset..].as_ptr() as *const __m256i);

        let cmp = _mm256_cmpeq_epi8(hay_vec, needle_vec);
        let mask = _mm256_movemask_epi8(cmp);

        // If not all bytes match, return false
        // Note: mask == -1 means all 32 bytes matched (all bits set)
        if mask != -1 {
            return false;
        }

        offset += 32;
    }

    // Handle remaining bytes (scalar fallback for tail)
    haystack[pos + offset..pos + needle.len()] == needle[offset..]
}

/// Single-byte search using AVX2 (specialized fast path)
///
/// # Safety
/// Requires AVX2 support, caller must ensure it's available
#[target_feature(enable = "avx2")]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr
)]
unsafe fn search_single_byte_avx2(haystack: &[u8], byte: u8) -> SearchResult {
    let byte_vec = _mm256_set1_epi8(byte as i8);
    let mut pos = 0;

    // Process 32 bytes at a time
    while pos + 32 <= haystack.len() {
        let hay_vec = _mm256_loadu_si256(haystack[pos..].as_ptr() as *const __m256i);
        let cmp = _mm256_cmpeq_epi8(hay_vec, byte_vec);
        let mask = _mm256_movemask_epi8(cmp);

        if mask != 0 {
            // Found match - return position of first set bit
            return Some(pos + mask.trailing_zeros() as usize);
        }

        pos += 32;
    }

    // Handle tail with scalar search
    haystack[pos..]
        .iter()
        .position(|&b| b == byte)
        .map(|i| pos + i)
}

/// Check if all bytes in a slice are ASCII using AVX2 bulk comparison.
///
/// Scans 32 bytes at a time, extracting the high bit of each byte via
/// `_mm256_movemask_epi8`. Any set bit means a byte >= 128 (non-ASCII).
///
/// # Safety
/// Caller must ensure AVX2 is available.
#[target_feature(enable = "avx2")]
unsafe fn is_ascii_avx2(bytes: &[u8]) -> bool {
    let mut pos = 0;
    while pos + 32 <= bytes.len() {
        let chunk = _mm256_loadu_si256(bytes[pos..].as_ptr() as *const __m256i);
        // movemask extracts the high bit from each byte into a 32-bit mask.
        // For ASCII bytes (0x00–0x7F) the high bit is always 0.
        if _mm256_movemask_epi8(chunk) != 0 {
            return false;
        }
        pos += 32;
    }
    // Scalar check for the remaining tail bytes
    bytes[pos..].iter().all(u8::is_ascii)
}

/// AVX2 trigram extraction with SIMD-accelerated ASCII fast path
///
/// Extracts all unique 3-character sliding windows from text.
///
/// **Fast path** (ASCII-only text): Uses AVX2 to verify all bytes < 128,
/// then operates on raw byte slices with packed `u32` dedup keys —
/// avoiding the `Vec<char>` allocation and per-trigram `String::from_iter`.
///
/// **Slow path** (non-ASCII text): Falls back to character-based extraction
/// for correct UTF-8 multi-byte handling.
///
/// # Safety
/// Caller must ensure AVX2 is available (checked by runtime detection in mod.rs)
#[target_feature(enable = "avx2")]
#[allow(clippy::must_use_candidate)]
#[must_use]
pub unsafe fn extract_trigrams(text: &str) -> Vec<String> {
    // Short-circuit: strings with fewer than 3 bytes cannot produce trigrams
    if text.len() < 3 {
        return vec![text.to_string()];
    }

    // Fast path: SIMD ASCII detection → byte-based extraction
    if is_ascii_avx2(text.as_bytes()) {
        return super::common::extract_trigrams_ascii_fast(text);
    }

    // Slow path: full Unicode character-based extraction
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return vec![text.to_string()];
    }

    let mut trigrams = Vec::with_capacity(chars.len().saturating_sub(2));
    let mut seen = HashSet::new();

    for i in 0..=chars.len().saturating_sub(3) {
        let trigram: String = chars[i..i + 3].iter().collect();
        if seen.insert(trigram.clone()) {
            trigrams.push(trigram);
        }
    }

    trigrams
}

/// AVX2 ASCII lowercase conversion using range check and masked addition
///
/// Converts A-Z to a-z in batches of 32 bytes using SIMD.
/// Non-ASCII UTF-8 characters are preserved unchanged.
///
/// # Algorithm
/// 1. Load 32 bytes at a time
/// 2. Check if each byte is in range [A-Z]
/// 3. Add 32 to uppercase bytes (A-Z → a-z)
/// 4. Leave all other bytes unchanged
///
/// # Safety
/// Caller must ensure AVX2 is available (checked by runtime detection in mod.rs)
///
/// # Performance
/// Expected ~4x speedup vs scalar for ASCII-heavy strings
#[target_feature(enable = "avx2")]
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_ptr_alignment,
    clippy::ptr_as_ptr,
    clippy::must_use_candidate
)]
#[must_use]
pub unsafe fn to_lowercase_ascii(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());

    // SIMD constants for A-Z range check
    let upper_a = _mm256_set1_epi8(b'A' as i8);
    let upper_z = _mm256_set1_epi8(b'Z' as i8);
    let to_lower_offset = _mm256_set1_epi8(32); // 'a' - 'A' = 32

    let mut pos = 0;

    // Process 32 bytes at a time
    while pos + 32 <= bytes.len() {
        // Load 32 bytes
        let chunk = _mm256_loadu_si256(bytes[pos..].as_ptr() as *const __m256i);

        // Check if each byte is >= 'A'
        // _mm256_cmpgt_epi8(a, b) returns 0xFF if a > b, 0x00 otherwise
        // We want >= 'A', so we compare chunk > ('A' - 1)
        let ge_a = _mm256_cmpgt_epi8(chunk, _mm256_sub_epi8(upper_a, _mm256_set1_epi8(1)));

        // Check if each byte is <= 'Z'
        // We want <= 'Z', so we compare ('Z' + 1) > chunk
        let le_z = _mm256_cmpgt_epi8(_mm256_add_epi8(upper_z, _mm256_set1_epi8(1)), chunk);

        // Combine: is_upper = (byte >= 'A') AND (byte <= 'Z')
        let is_upper = _mm256_and_si256(ge_a, le_z);

        // Convert: add 32 to uppercase bytes, leave others unchanged
        // is_upper is 0xFF for uppercase, 0x00 for others
        // AND with offset gives 32 for uppercase, 0 for others
        let offset_masked = _mm256_and_si256(is_upper, to_lower_offset);
        let lowercased = _mm256_add_epi8(chunk, offset_masked);

        // Store result
        let mut temp = [0u8; 32];
        _mm256_storeu_si256(temp.as_mut_ptr() as *mut __m256i, lowercased);
        result.extend_from_slice(&temp);

        pos += 32;
    }

    // Scalar fallback for tail
    for &byte in &bytes[pos..] {
        result.push(byte.to_ascii_lowercase());
    }

    // SAFETY: We only modified ASCII bytes (A-Z → a-z), UTF-8 remains valid
    String::from_utf8_unchecked(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    // AVX2-specific tests (TS-2.2: AVX2 implementation tests)
    // These tests verify AVX2 implementation correctness
    // Property tests in mod.rs will verify AVX2 ≡ scalar equivalence

    #[test]
    fn test_avx2_search_basic() {
        unsafe {
            assert_eq!(search(b"hello world", b"world"), Some(6));
            assert_eq!(search(b"hello", b"xyz"), None);
        }
    }

    #[test]
    fn test_avx2_search_single_byte() {
        unsafe {
            assert_eq!(search(b"hello", b"h"), Some(0));
            assert_eq!(search(b"hello", b"o"), Some(4));
            assert_eq!(search(b"hello", b"x"), None);
        }
    }

    #[test]
    fn test_avx2_search_long_haystack() {
        unsafe {
            let haystack = b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            assert_eq!(search(haystack, b"xyz"), Some(23));
            assert_eq!(search(haystack, b"XYZ"), Some(59));
        }
    }

    #[test]
    fn test_avx2_search_repeated_pattern() {
        unsafe {
            // Should return first match
            assert_eq!(search(b"aaaaaaaaaa", b"aa"), Some(0));
        }
    }

    #[test]
    fn test_avx2_lowercase_basic() {
        unsafe {
            assert_eq!(to_lowercase_ascii("HELLO"), "hello");
            assert_eq!(to_lowercase_ascii("HeLLo"), "hello");
            assert_eq!(to_lowercase_ascii("hello"), "hello");
        }
    }

    #[test]
    fn test_avx2_lowercase_long_string() {
        unsafe {
            let input = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            let expected = "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
            assert_eq!(to_lowercase_ascii(input), expected);
        }
    }

    #[test]
    fn test_avx2_lowercase_mixed() {
        unsafe {
            assert_eq!(to_lowercase_ascii("ABC123xyz"), "abc123xyz");
        }
    }

    #[test]
    fn test_avx2_trigram_basic() {
        unsafe {
            let mut trigrams = extract_trigrams("hello");
            trigrams.sort();
            assert_eq!(trigrams, vec!["ell", "hel", "llo"]);
        }
    }

    #[test]
    fn test_avx2_trigram_short() {
        unsafe {
            assert_eq!(extract_trigrams("ab"), vec!["ab"]);
            assert_eq!(extract_trigrams("abc"), vec!["abc"]);
        }
    }

    #[test]
    fn test_avx2_trigram_ascii_fast_path() {
        unsafe {
            // Pure ASCII — exercises the SIMD ASCII fast path
            let mut trigrams = extract_trigrams("abcdef");
            trigrams.sort();
            assert_eq!(trigrams, vec!["abc", "bcd", "cde", "def"]);
        }
    }

    #[test]
    fn test_avx2_trigram_ascii_long() {
        unsafe {
            // Long enough to span multiple 32-byte AVX2 lanes
            let input = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP";
            let trigrams = extract_trigrams(input);
            // 52 chars → 50 unique trigrams
            assert_eq!(trigrams.len(), 50);
            assert_eq!(trigrams[0], "abc");
            assert_eq!(trigrams[49], "NOP");
        }
    }

    #[test]
    fn test_avx2_trigram_non_ascii_fallback() {
        unsafe {
            // Non-ASCII — must fall back to char-based extraction
            let mut trigrams = extract_trigrams("héllo");
            trigrams.sort();
            // chars: h, é, l, l, o → trigrams: "hél", "éll", "llo"
            assert_eq!(trigrams.len(), 3);
            assert!(trigrams.contains(&"hél".to_string()));
            assert!(trigrams.contains(&"éll".to_string()));
            assert!(trigrams.contains(&"llo".to_string()));
        }
    }

    #[test]
    fn test_avx2_trigram_dedup() {
        unsafe {
            // Repeated chars produce duplicate trigrams — verify dedup
            let trigrams = extract_trigrams("aaaa");
            assert_eq!(trigrams, vec!["aaa"]);
        }
    }

    #[test]
    fn test_avx2_is_ascii() {
        unsafe {
            assert!(is_ascii_avx2(b"hello world"));
            assert!(is_ascii_avx2(b""));
            assert!(is_ascii_avx2(
                b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
            ));
            assert!(!is_ascii_avx2("héllo".as_bytes()));
            assert!(!is_ascii_avx2("hello 世界".as_bytes()));
            // Non-ASCII at end (tail path)
            assert!(!is_ascii_avx2(
                "abcdefghijklmnopqrstuvwxyz012345é".as_bytes()
            ));
        }
    }
}
