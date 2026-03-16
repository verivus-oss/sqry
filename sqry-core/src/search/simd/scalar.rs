//! Scalar (non-SIMD) fallback implementations
//!
//! This module provides reference implementations of search operations
//! without SIMD optimization. These serve as:
//! - Fallback for CPUs without SIMD support
//! - Reference for property testing (SIMD ≡ scalar validation)
//! - Baseline for performance comparisons

use super::SearchResult;
use std::collections::HashSet;

/// Search for needle in haystack using Boyer-Moore-Horspool algorithm
///
/// Returns the byte offset of the first match, or None if not found.
///
/// # Algorithm
/// Boyer-Moore-Horspool (BMH) is a simplified version of Boyer-Moore that:
/// - Builds a skip table based on the needle
/// - Scans haystack left-to-right, but checks pattern right-to-left
/// - Skips ahead based on mismatched character
///
/// Time complexity: O(n*m) worst case, O(n/m) average case
/// Space complexity: O(256) for skip table
///
/// # Examples
/// ```
/// use sqry_core::search::simd::scalar::search;
///
/// assert_eq!(search(b"hello world", b"world"), Some(6));
/// assert_eq!(search(b"hello", b"xyz"), None);
/// ```
#[must_use]
pub fn search(haystack: &[u8], needle: &[u8]) -> SearchResult {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }

    // Special case: single-byte needle (optimize common case)
    if needle.len() == 1 {
        let target = needle[0];
        return haystack.iter().position(|&b| b == target);
    }

    // Build skip table for Boyer-Moore-Horspool
    let skip_table = build_skip_table(needle);
    let needle_len = needle.len();
    let last_idx = needle_len - 1;

    let mut pos = 0;
    while pos <= haystack.len() - needle_len {
        // Check if pattern matches at current position (right-to-left)
        let mut j = last_idx;
        loop {
            if haystack[pos + j] != needle[j] {
                // Mismatch: skip ahead based on last character
                let skip_char = haystack[pos + last_idx];
                pos += skip_table[skip_char as usize];
                break;
            }

            if j == 0 {
                // Full match found
                return Some(pos);
            }
            j -= 1;
        }
    }

    None
}

/// Build skip table for Boyer-Moore-Horspool algorithm
///
/// The skip table tells us how far we can skip ahead when we find a mismatch.
/// For each character, we store the distance from the end of the needle.
///
/// # Example
/// For needle "hello":
/// - 'h' → skip 4 (distance from end)
/// - 'e' → skip 3
/// - 'l' → skip 1 (rightmost 'l')
/// - 'o' → skip 0 (last character)
/// - all other chars → skip 5 (`needle.len()`)
fn build_skip_table(needle: &[u8]) -> [usize; 256] {
    let mut table = [needle.len(); 256];
    let last_idx = needle.len() - 1;

    for (i, &byte) in needle.iter().enumerate().take(last_idx) {
        table[byte as usize] = last_idx - i;
    }

    table
}

/// Extract trigrams from text using sliding window
///
/// A trigram is a 3-character substring. This function extracts all unique
/// trigrams using a sliding window approach.
///
/// For strings shorter than 3 characters, returns the original string.
///
/// # Examples
/// ```
/// use sqry_core::search::simd::scalar::extract_trigrams;
///
/// assert_eq!(extract_trigrams("hello"), vec!["hel", "ell", "llo"]);
/// assert_eq!(extract_trigrams("ab"), vec!["ab"]);
/// ```
#[must_use]
pub fn extract_trigrams(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();

    if chars.len() < 3 {
        return vec![text.to_string()];
    }

    let mut trigrams = Vec::new();
    let mut seen = HashSet::new();

    for i in 0..=chars.len() - 3 {
        let trigram: String = chars[i..i + 3].iter().collect();
        if seen.insert(trigram.clone()) {
            trigrams.push(trigram);
        }
    }

    trigrams
}

/// Convert ASCII text to lowercase
///
/// Only ASCII characters (A-Z) are converted to lowercase (a-z).
/// Non-ASCII characters and already-lowercase characters are preserved.
///
/// # Examples
/// ```
/// use sqry_core::search::simd::scalar::to_lowercase_ascii;
///
/// assert_eq!(to_lowercase_ascii("HELLO"), "hello");
/// assert_eq!(to_lowercase_ascii("Hello123"), "hello123");
/// ```
#[must_use]
pub fn to_lowercase_ascii(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_uppercase() {
                c.to_ascii_lowercase()
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

    const MAX_HAYSTACK_LEN: usize = 256;
    const MAX_NEEDLE_LEN: usize = 96;
    const MAX_EXTRA_LEN: usize = 128;

    // Substring search tests (TS-1.1)
    #[test]
    fn test_search_basic_match() {
        assert_eq!(search(b"hello", b"ll"), Some(2));
    }

    #[test]
    fn test_search_no_match() {
        assert_eq!(search(b"hello", b"world"), None);
    }

    #[test]
    fn test_search_empty_haystack() {
        assert_eq!(search(b"", b"x"), None);
    }

    #[test]
    fn test_search_empty_needle() {
        assert_eq!(search(b"hello", b""), Some(0));
    }

    #[test]
    fn test_search_repeated_pattern() {
        // Should return first match
        assert_eq!(search(b"aaaaaa", b"aa"), Some(0));
    }

    #[test]
    fn test_search_single_byte_start() {
        assert_eq!(search(b"hello", b"h"), Some(0));
    }

    #[test]
    fn test_search_single_byte_end() {
        assert_eq!(search(b"hello", b"o"), Some(4));
    }

    #[test]
    fn test_search_match_at_end() {
        assert_eq!(search(b"hello world", b"world"), Some(6));
    }

    #[test]
    fn test_search_large_haystack_no_match() {
        let haystack = "abcdefgh".repeat(1000);
        assert_eq!(search(haystack.as_bytes(), b"xyz"), None);
    }

    #[test]
    fn test_search_large_haystack_match() {
        let haystack = "abc".repeat(1000) + "xyz";
        let result = search(haystack.as_bytes(), b"xyz");
        assert!(result.is_some());
        assert_eq!(
            &haystack.as_bytes()[result.unwrap()..result.unwrap() + 3],
            b"xyz"
        );
    }

    #[test]
    fn test_search_exact_match() {
        assert_eq!(search(b"hello", b"hello"), Some(0));
    }

    #[test]
    fn test_search_needle_longer() {
        assert_eq!(search(b"hello", b"hello world"), None);
    }

    // Trigram extraction tests (TS-1.2)
    #[test]
    fn test_trigram_single() {
        assert_eq!(extract_trigrams("abc"), vec!["abc"]);
    }

    #[test]
    fn test_trigram_two() {
        let mut trigrams = extract_trigrams("abcd");
        trigrams.sort();
        assert_eq!(trigrams, vec!["abc", "bcd"]);
    }

    #[test]
    fn test_trigram_too_short() {
        assert_eq!(extract_trigrams("ab"), vec!["ab"]);
    }

    #[test]
    fn test_trigram_empty() {
        assert_eq!(extract_trigrams(""), vec![""]);
    }

    #[test]
    fn test_trigram_repeated_chars() {
        assert_eq!(extract_trigrams("aaa"), vec!["aaa"]);
    }

    #[test]
    fn test_trigram_full_sliding_window() {
        let mut trigrams = extract_trigrams("abcdefgh");
        trigrams.sort();
        assert_eq!(trigrams, vec!["abc", "bcd", "cde", "def", "efg", "fgh"]);
    }

    #[test]
    fn test_trigram_realistic_symbol() {
        let trigrams = extract_trigrams("createCompilerHost");
        // Should extract 16 unique trigrams (length - 2)
        assert_eq!(trigrams.len(), 16);
        assert!(trigrams.contains(&"cre".to_string()));
        assert!(trigrams.contains(&"rea".to_string()));
        assert!(trigrams.contains(&"ate".to_string()));
    }

    #[test]
    fn test_trigram_large_input() {
        let input = "a".repeat(1000);
        let trigrams = extract_trigrams(&input);
        // All trigrams are "aaa", so only 1 unique
        assert_eq!(trigrams.len(), 1);
        assert_eq!(trigrams[0], "aaa");
    }

    // ASCII lowercase tests (TS-1.3)
    #[test]
    fn test_lowercase_all_uppercase() {
        assert_eq!(to_lowercase_ascii("HELLO"), "hello");
    }

    #[test]
    fn test_lowercase_all_lowercase() {
        assert_eq!(to_lowercase_ascii("hello"), "hello");
    }

    #[test]
    fn test_lowercase_mixed_case() {
        assert_eq!(to_lowercase_ascii("HeLLo"), "hello");
    }

    #[test]
    fn test_lowercase_alphanumeric() {
        assert_eq!(to_lowercase_ascii("ABC123XYZ"), "abc123xyz");
    }

    #[test]
    fn test_lowercase_empty() {
        assert_eq!(to_lowercase_ascii(""), "");
    }

    #[test]
    fn test_lowercase_realistic_symbol() {
        assert_eq!(
            to_lowercase_ascii("createCompilerHost"),
            "createcompilerhost"
        );
    }

    // Skip table tests
    #[test]
    fn test_build_skip_table() {
        let table = build_skip_table(b"hello");

        // 'h' is at position 0, distance from end (index 4) = 4
        assert_eq!(table[b'h' as usize], 4);

        // 'e' is at position 1, distance from end = 3
        assert_eq!(table[b'e' as usize], 3);

        // 'l' appears at positions 2 and 3, rightmost is 3, distance = 1
        assert_eq!(table[b'l' as usize], 1);

        // 'o' is at last position (index 4), not in skip table (would be 0)
        // But we only add chars up to last_idx-1, so 'o' gets default value
        assert_eq!(table[b'o' as usize], 5); // needle.len()

        // Character not in needle gets default (needle.len())
        assert_eq!(table[b'x' as usize], 5);
    }

    // Property tests for scalar baseline (TS-2.1)
    // These establish correctness guarantees that SIMD implementations must match

    proptest! {
        #[test]
        fn prop_search_finds_substr_if_present(
            haystack in vec(any::<u8>(), 0..=MAX_HAYSTACK_LEN),
            needle in vec(any::<u8>(), 0..=MAX_NEEDLE_LEN),
        ) {
            prop_assume!(!needle.is_empty());

            let result = search(&haystack, &needle);
            if let Some(pos) = result {
                prop_assert!(pos + needle.len() <= haystack.len());
                prop_assert_eq!(&haystack[pos..pos + needle.len()], needle.as_slice());
            } else {
                prop_assert!(!haystack.windows(needle.len()).any(|w| w == needle.as_slice()));
            }
        }

        #[test]
        fn prop_search_empty_needle_returns_zero(
            haystack in vec(any::<u8>(), 0..=MAX_HAYSTACK_LEN)
        ) {
            prop_assert_eq!(search(&haystack, &[]), Some(0));
        }

        #[test]
        fn prop_search_needle_longer_returns_none(
            haystack in vec(any::<u8>(), 0..=MAX_HAYSTACK_LEN),
            extra in vec(any::<u8>(), 1..=MAX_EXTRA_LEN),
        ) {
            let mut needle = haystack.clone();
            needle.extend(extra.iter());
            prop_assert!(needle.len() > haystack.len());
            prop_assert!(search(&haystack, &needle).is_none());
        }

        #[test]
        fn prop_trigram_length_correct(text in any::<String>()) {
            let trigrams = extract_trigrams(&text);
            let char_count = text.chars().count();

            if char_count < 3 {
                prop_assert_eq!(trigrams.len(), 1);
                prop_assert_eq!(trigrams[0].as_str(), text.as_str());
            } else {
                prop_assert!(trigrams.iter().all(|t| t.chars().count() == 3));
            }
        }

        #[test]
        fn prop_trigram_count_bounded(text in any::<String>()) {
            let trigrams = extract_trigrams(&text);
            let char_count = text.chars().count();

            if char_count < 3 {
                prop_assert_eq!(trigrams.len(), 1);
            } else {
                prop_assert!(trigrams.len() <= char_count.saturating_sub(2));
            }
        }

        #[test]
        fn prop_lowercase_preserves_length(text in any::<String>()) {
            if text.is_ascii() {
                let lower = to_lowercase_ascii(&text);
                prop_assert_eq!(lower.len(), text.len());
            }
        }

        #[test]
        fn prop_lowercase_idempotent(text in any::<String>()) {
            let lower1 = to_lowercase_ascii(&text);
            let lower2 = to_lowercase_ascii(&lower1);
            prop_assert_eq!(lower1, lower2);
        }

        #[test]
        fn prop_lowercase_only_affects_ascii_uppercase(text in any::<String>()) {
            let lower = to_lowercase_ascii(&text);
            let only_ascii_changes = text.chars().zip(lower.chars()).all(|(orig, lc)| {
                if orig.is_ascii_uppercase() {
                    lc == orig.to_ascii_lowercase()
                } else {
                    lc == orig
                }
            });
            prop_assert!(only_ascii_changes);
        }
    }
}
