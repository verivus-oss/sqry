//! NEON-accelerated search operations (ARM64 only)
//!
//! This module provides NEON (16-byte vector) implementations of:
//! - Substring search using vceqq_u8
//! - Trigram extraction using vld1q_u8
//! - ASCII lowercase using vcgtq_u8
//!
//! All functions require NEON support (runtime detection in mod.rs).
//! Falls back to scalar implementation if NEON unavailable.

use super::SearchResult;
use super::common::{
    advance_step, build_skip_table, early_search_result, find_match_in_mask, tail_match,
};
use std::arch::aarch64::*;
use std::collections::HashSet;

const LANE_BYTES: usize = 16;

/// NEON substring search using Boyer-Moore-Horspool with SIMD first-byte scan
///
/// # Algorithm
/// 1. Build BMH skip table for efficient skipping
/// 2. Use NEON to scan 16 bytes at once for first-byte matches
/// 3. Verify candidates with SIMD memcmp
/// 4. Skip ahead using BMH table on mismatch
///
/// # Safety
/// Caller must ensure NEON is available (checked by runtime detection in mod.rs)
///
/// # Performance
/// Expected 2-3x speedup vs scalar for needles >8 bytes
#[target_feature(enable = "neon")]
pub unsafe fn search(haystack: &[u8], needle: &[u8]) -> SearchResult {
    // SAFETY: Caller guarantees NEON is available; delegates to unsafe helpers.
    unsafe {
        if let Some(result) = early_search_result(haystack, needle, search_single_byte_neon) {
            return result;
        }

        // Build skip table for Boyer-Moore-Horspool
        let skip_table = build_skip_table(needle);
        let needle_len = needle.len();

        // Broadcast first byte of needle to all lanes
        let first_vec = vdupq_n_u8(needle[0]);

        let mut pos = 0;
        while pos <= haystack.len().saturating_sub(needle_len) {
            let at_tail = pos + LANE_BYTES > haystack.len();
            if let Some(candidate_pos) =
                scan_window_neon(haystack, needle, pos, needle_len, first_vec, at_tail)
            {
                return Some(candidate_pos);
            }

            // BMH skip: advance by skip distance based on last character in window
            pos += advance_step(haystack, needle_len, pos, &skip_table, at_tail);
        }

        None
    }
}

#[target_feature(enable = "neon")]
unsafe fn scan_window_neon(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    needle_len: usize,
    first_vec: uint8x16_t,
    at_tail: bool,
) -> Option<usize> {
    // SAFETY: Caller guarantees NEON and valid positions.
    unsafe {
        if at_tail {
            return tail_match(haystack, needle, pos, needle_len);
        }

        scan_candidates_neon(haystack, needle, pos, needle_len, first_vec)
    }
}

#[target_feature(enable = "neon")]
unsafe fn scan_candidates_neon(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    needle_len: usize,
    first_vec: uint8x16_t,
) -> Option<usize> {
    // SAFETY: Caller guarantees NEON and valid haystack slice; vld1q_u8 pointer is in-bounds.
    unsafe {
        // Load 16 bytes from haystack at position pos
        let hay_vec = vld1q_u8(haystack[pos..].as_ptr());

        // Compare first byte: find all positions where first byte matches
        let first_cmp = vceqq_u8(hay_vec, first_vec);

        // Extract bitmask from comparison result
        let first_mask = neon_movemask(first_cmp);

        if first_mask == 0 {
            return None;
        }

        find_match_in_mask(
            haystack,
            needle,
            pos,
            needle_len,
            first_mask as u32,
            LANE_BYTES,
            verify_match_neon,
        )
    }
}

/// Extract bitmask from NEON comparison result
///
/// NEON doesn't have a direct equivalent to x86's _mm_movemask_epi8,
/// so we need to manually extract the high bit from each byte.
///
/// # Algorithm
/// For each byte in the 16-byte vector, extract the high bit (0x80)
/// and shift it to create a 16-bit mask.
#[inline]
unsafe fn neon_movemask(v: uint8x16_t) -> u16 {
    // SAFETY: vshrq_n_u8 requires target_feature; transmute converts matching-size types.
    unsafe {
        // Extract high bit from each byte using right shift + narrow
        // This creates a more efficient implementation than checking each lane individually

        // Method: Use vshrn to extract high bits efficiently
        // First, shift right by 7 to get high bits in LSB position
        let shift = vshrq_n_u8::<7>(v);

        // Extract individual bytes and build mask
        // NEON provides vgetq_lane_u8 to extract individual lanes
        // Proper implementation: extract all lanes efficiently
        let bytes: [u8; 16] = std::mem::transmute(shift);
        let mut result = 0u16;
        for i in 0..16 {
            result |= ((bytes[i] & 1) as u16) << i;
        }
        result
    }
}

/// Verify full needle match at candidate position using NEON
///
/// # Safety
/// Requires NEON support, caller must ensure it's available
#[target_feature(enable = "neon")]
unsafe fn verify_match_neon(haystack: &[u8], needle: &[u8], pos: usize) -> bool {
    // SAFETY: Caller guarantees NEON and valid positions; vld1q_u8 pointers are in-bounds.
    unsafe {
        let mut offset = 0;

        // Process 16 bytes at a time
        while offset + 16 <= needle.len() {
            let hay_vec = vld1q_u8(haystack[pos + offset..].as_ptr());
            let needle_vec = vld1q_u8(needle[offset..].as_ptr());

            let cmp = vceqq_u8(hay_vec, needle_vec);

            // Check if all bytes matched
            // Convert to u64 lanes and check if all bits are set
            let cmp64 = vreinterpretq_u64_u8(cmp);
            let low = vgetq_lane_u64::<0>(cmp64);
            let high = vgetq_lane_u64::<1>(cmp64);

            // If all bytes match, both lanes should be 0xFFFFFFFFFFFFFFFF
            if low != 0xFFFFFFFFFFFFFFFF || high != 0xFFFFFFFFFFFFFFFF {
                return false;
            }

            offset += 16;
        }

        // Handle remaining bytes (scalar fallback for tail)
        haystack[pos + offset..pos + needle.len()] == needle[offset..]
    }
}

/// Single-byte search using NEON (specialized fast path)
///
/// # Safety
/// Requires NEON support, caller must ensure it's available
#[target_feature(enable = "neon")]
unsafe fn search_single_byte_neon(haystack: &[u8], byte: u8) -> SearchResult {
    // SAFETY: Caller guarantees NEON; vld1q_u8 pointers are in-bounds due to length check.
    unsafe {
        let byte_vec = vdupq_n_u8(byte);
        let mut pos = 0;

        // Process 16 bytes at a time
        while pos + 16 <= haystack.len() {
            let hay_vec = vld1q_u8(haystack[pos..].as_ptr());
            let cmp = vceqq_u8(hay_vec, byte_vec);
            let mask = neon_movemask(cmp);

            if mask != 0 {
                // Found match - return position of first set bit
                return Some(pos + mask.trailing_zeros() as usize);
            }

            pos += 16;
        }

        // Handle tail with scalar search
        haystack[pos..]
            .iter()
            .position(|&b| b == byte)
            .map(|i| pos + i)
    }
}

/// Check if all bytes in a slice are ASCII using NEON bulk comparison.
///
/// Scans 16 bytes at a time, comparing each byte against 0x80 (unsigned >=).
/// Any byte >= 128 is non-ASCII.
///
/// # Safety
/// Caller must ensure NEON is available.
#[target_feature(enable = "neon")]
unsafe fn is_ascii_neon(bytes: &[u8]) -> bool {
    // SAFETY: Caller guarantees NEON; vld1q_u8 pointers are in-bounds due to length check.
    unsafe {
        let mut pos = 0;
        let threshold = vdupq_n_u8(0x80);
        while pos + 16 <= bytes.len() {
            let chunk = vld1q_u8(bytes[pos..].as_ptr());
            // vcgeq_u8: unsigned >= comparison, returns 0xFF for bytes >= 0x80
            let cmp = vcgeq_u8(chunk, threshold);
            if neon_movemask(cmp) != 0 {
                return false;
            }
            pos += 16;
        }
        // Scalar check for the remaining tail bytes
        bytes[pos..].iter().all(u8::is_ascii)
    }
}

/// NEON trigram extraction with SIMD-accelerated ASCII fast path
///
/// Extracts all unique 3-character sliding windows from text.
///
/// **Fast path** (ASCII-only text): Uses NEON to verify all bytes < 128,
/// then operates on raw byte slices with packed `u32` dedup keys —
/// avoiding the `Vec<char>` allocation and per-trigram `String::from_iter`.
///
/// **Slow path** (non-ASCII text): Falls back to character-based extraction
/// for correct UTF-8 multi-byte handling.
///
/// # Safety
/// Caller must ensure NEON is available (checked by runtime detection in mod.rs)
#[target_feature(enable = "neon")]
pub unsafe fn extract_trigrams(text: &str) -> Vec<String> {
    // Short-circuit: strings with fewer than 3 bytes cannot produce trigrams
    if text.len() < 3 {
        return vec![text.to_string()];
    }

    // Fast path: SIMD ASCII detection → byte-based extraction
    // SAFETY: Caller guarantees NEON; is_ascii_neon is an unsafe fn requiring NEON.
    if unsafe { is_ascii_neon(text.as_bytes()) } {
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

/// NEON ASCII lowercase conversion using range check and masked addition
///
/// Converts A-Z to a-z in batches of 16 bytes using SIMD.
/// Non-ASCII UTF-8 characters are preserved unchanged.
///
/// # Algorithm
/// 1. Load 16 bytes at a time
/// 2. Check if each byte is in range [A-Z]
/// 3. Add 32 to uppercase bytes (A-Z → a-z)
/// 4. Leave all other bytes unchanged
///
/// # Safety
/// Caller must ensure NEON is available (checked by runtime detection in mod.rs)
///
/// # Performance
/// Expected ~3x speedup vs scalar for ASCII-heavy strings
#[target_feature(enable = "neon")]
pub unsafe fn to_lowercase_ascii(text: &str) -> String {
    // SAFETY: Caller guarantees NEON; pointer ops via vld1q_u8/vst1q_u8 are in-bounds
    // due to length checks; from_utf8_unchecked is safe because only A-Z→a-z is modified.
    unsafe {
        let bytes = text.as_bytes();
        let mut result = Vec::with_capacity(bytes.len());

        // SIMD constants for A-Z range check
        let upper_a = vdupq_n_u8(b'A');
        let upper_z = vdupq_n_u8(b'Z');
        let to_lower_offset = vdupq_n_u8(32); // 'a' - 'A' = 32

        let mut pos = 0;

        // Process 16 bytes at a time
        while pos + 16 <= bytes.len() {
            // Load 16 bytes
            let chunk = vld1q_u8(bytes[pos..].as_ptr());

            // Check if each byte is >= 'A'
            // vcgeq_u8(a, b) returns 0xFF if a >= b, 0x00 otherwise
            let ge_a = vcgeq_u8(chunk, upper_a);

            // Check if each byte is <= 'Z'
            let le_z = vcleq_u8(chunk, upper_z);

            // Combine: is_upper = (byte >= 'A') AND (byte <= 'Z')
            let is_upper = vandq_u8(ge_a, le_z);

            // Convert: add 32 to uppercase bytes, leave others unchanged
            // is_upper is 0xFF for uppercase, 0x00 for others
            // AND with offset gives 32 for uppercase, 0 for others
            let offset_masked = vandq_u8(is_upper, to_lower_offset);
            let lowercased = vaddq_u8(chunk, offset_masked);

            // Store result
            let mut temp = [0u8; 16];
            vst1q_u8(temp.as_mut_ptr(), lowercased);
            result.extend_from_slice(&temp);

            pos += 16;
        }

        // Scalar fallback for tail
        for &byte in &bytes[pos..] {
            result.push(byte.to_ascii_lowercase());
        }

        // We only modified ASCII bytes (A-Z → a-z), UTF-8 remains valid
        String::from_utf8_unchecked(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NEON-specific tests (TS-3.2: NEON implementation tests)
    // These tests verify NEON implementation correctness
    // Property tests in mod.rs will verify NEON ≡ scalar equivalence

    #[test]
    fn test_neon_search_basic() {
        unsafe {
            assert_eq!(search(b"hello world", b"world"), Some(6));
            assert_eq!(search(b"hello", b"xyz"), None);
        }
    }

    #[test]
    fn test_neon_search_single_byte() {
        unsafe {
            assert_eq!(search(b"hello", b"h"), Some(0));
            assert_eq!(search(b"hello", b"o"), Some(4));
            assert_eq!(search(b"hello", b"x"), None);
        }
    }

    #[test]
    fn test_neon_search_long_haystack() {
        unsafe {
            let haystack = b"abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            assert_eq!(search(haystack, b"xyz"), Some(23));
            assert_eq!(search(haystack, b"XYZ"), Some(59));
        }
    }

    #[test]
    fn test_neon_search_repeated_pattern() {
        unsafe {
            // Should return first match
            assert_eq!(search(b"aaaaaaaaaa", b"aa"), Some(0));
        }
    }

    #[test]
    fn test_neon_lowercase_basic() {
        unsafe {
            assert_eq!(to_lowercase_ascii("HELLO"), "hello");
            assert_eq!(to_lowercase_ascii("HeLLo"), "hello");
            assert_eq!(to_lowercase_ascii("hello"), "hello");
        }
    }

    #[test]
    fn test_neon_lowercase_long_string() {
        unsafe {
            let input = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
            let expected = "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz";
            assert_eq!(to_lowercase_ascii(input), expected);
        }
    }

    #[test]
    fn test_neon_lowercase_mixed() {
        unsafe {
            assert_eq!(to_lowercase_ascii("ABC123xyz"), "abc123xyz");
        }
    }

    #[test]
    fn test_neon_trigram_basic() {
        unsafe {
            let mut trigrams = extract_trigrams("hello");
            trigrams.sort();
            assert_eq!(trigrams, vec!["ell", "hel", "llo"]);
        }
    }

    #[test]
    fn test_neon_trigram_short() {
        unsafe {
            assert_eq!(extract_trigrams("ab"), vec!["ab"]);
            assert_eq!(extract_trigrams("abc"), vec!["abc"]);
        }
    }

    #[test]
    fn test_neon_trigram_ascii_fast_path() {
        unsafe {
            // Pure ASCII — exercises the SIMD ASCII fast path
            let mut trigrams = extract_trigrams("abcdef");
            trigrams.sort();
            assert_eq!(trigrams, vec!["abc", "bcd", "cde", "def"]);
        }
    }

    #[test]
    fn test_neon_trigram_ascii_long() {
        unsafe {
            // Long enough to span multiple 16-byte NEON lanes
            let input = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOP";
            let trigrams = extract_trigrams(input);
            // 52 chars → 50 unique trigrams
            assert_eq!(trigrams.len(), 50);
            assert_eq!(trigrams[0], "abc");
            assert_eq!(trigrams[49], "NOP");
        }
    }

    #[test]
    fn test_neon_trigram_non_ascii_fallback() {
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
    fn test_neon_trigram_dedup() {
        unsafe {
            // Repeated chars produce duplicate trigrams — verify dedup
            let trigrams = extract_trigrams("aaaa");
            assert_eq!(trigrams, vec!["aaa"]);
        }
    }

    #[test]
    fn test_neon_is_ascii() {
        unsafe {
            assert!(is_ascii_neon(b"hello world"));
            assert!(is_ascii_neon(b""));
            assert!(is_ascii_neon(
                b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
            ));
            assert!(!is_ascii_neon("héllo".as_bytes()));
            assert!(!is_ascii_neon("hello 世界".as_bytes()));
            // Non-ASCII at end (tail path)
            assert!(!is_ascii_neon("abcdefghijklmnopé".as_bytes()));
        }
    }
}
