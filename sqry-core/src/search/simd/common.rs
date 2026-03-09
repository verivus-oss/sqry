use super::SearchResult;
use std::collections::HashSet;

/// Extract trigrams from ASCII-only text using direct byte slices.
///
/// When all bytes are ASCII, char boundaries == byte boundaries, so we can:
/// 1. Skip the `Vec<char>` allocation (4 bytes/char → 1 byte/char)
/// 2. Use packed `u32` keys in the `HashSet` instead of `String` keys
/// 3. Avoid per-trigram `String::from_iter` (use `from_utf8_unchecked` on byte slices)
///
/// # Safety contract
/// Caller **must** verify that `text` contains only ASCII bytes (all < 128).
/// In debug builds this is checked by `debug_assert`.
pub(super) fn extract_trigrams_ascii_fast(text: &str) -> Vec<String> {
    debug_assert!(
        text.is_ascii(),
        "extract_trigrams_ascii_fast requires ASCII-only input"
    );
    let bytes = text.as_bytes();
    if bytes.len() < 3 {
        return vec![text.to_string()];
    }

    let capacity = bytes.len().saturating_sub(2);
    let mut trigrams = Vec::with_capacity(capacity);
    let mut seen = HashSet::with_capacity(capacity);

    for i in 0..=bytes.len().saturating_sub(3) {
        // Pack 3 ASCII bytes into a u32 for fast hashing and comparison
        let key =
            (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        if seen.insert(key) {
            // SAFETY: Caller verified all bytes are ASCII (< 128).
            // Any sequence of bytes in 0x00..=0x7F is valid UTF-8.
            let trigram = unsafe { std::str::from_utf8_unchecked(&bytes[i..i + 3]) };
            trigrams.push(trigram.to_string());
        }
    }

    trigrams
}

pub(super) unsafe fn early_search_result(
    haystack: &[u8],
    needle: &[u8],
    single_byte_search: unsafe fn(&[u8], u8) -> SearchResult,
) -> Option<SearchResult> {
    if needle.is_empty() {
        return Some(Some(0));
    }
    if haystack.len() < needle.len() {
        return Some(None);
    }
    if needle.len() == 1 {
        return Some(unsafe { single_byte_search(haystack, needle[0]) });
    }
    None
}

pub(super) fn tail_match(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    needle_len: usize,
) -> Option<usize> {
    if pos + needle_len <= haystack.len() && haystack[pos..pos + needle_len] == needle[..] {
        Some(pos)
    } else {
        None
    }
}

pub(super) fn advance_step(
    haystack: &[u8],
    needle_len: usize,
    pos: usize,
    skip_table: &[usize; 256],
    at_tail: bool,
) -> usize {
    if at_tail {
        1
    } else {
        skip_distance(haystack, needle_len, pos, skip_table)
    }
}

pub(super) fn build_skip_table(needle: &[u8]) -> [usize; 256] {
    let mut table = [needle.len(); 256];
    let last_idx = needle.len() - 1;

    for (i, &byte) in needle.iter().enumerate().take(last_idx) {
        table[byte as usize] = last_idx - i;
    }

    table
}

pub(super) unsafe fn find_match_in_mask(
    haystack: &[u8],
    needle: &[u8],
    pos: usize,
    needle_len: usize,
    mask: u32,
    lane_bytes: usize,
    verify_match: unsafe fn(&[u8], &[u8], usize) -> bool,
) -> Option<usize> {
    for bit_pos in 0..lane_bytes {
        let Ok(bit_pos_u32) = u32::try_from(bit_pos) else {
            continue;
        };
        if (mask & (1u32 << bit_pos_u32)) != 0 {
            let candidate_pos = pos + bit_pos;

            if candidate_pos + needle_len > haystack.len() {
                break;
            }

            if unsafe { verify_match(haystack, needle, candidate_pos) } {
                return Some(candidate_pos);
            }
        }
    }

    None
}

fn skip_distance(
    haystack: &[u8],
    needle_len: usize,
    pos: usize,
    skip_table: &[usize; 256],
) -> usize {
    let skip_pos = pos + needle_len - 1;
    if skip_pos < haystack.len() {
        let skip_char = haystack[skip_pos];
        skip_table[skip_char as usize]
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::{build_skip_table, extract_trigrams_ascii_fast};

    #[test]
    fn test_build_skip_table() {
        let table = build_skip_table(b"hello");

        assert_eq!(table[b'h' as usize], 4);
        assert_eq!(table[b'e' as usize], 3);
        assert_eq!(table[b'l' as usize], 1);
        assert_eq!(table[b'o' as usize], 5);
        assert_eq!(table[b'x' as usize], 5);
    }

    #[test]
    fn test_ascii_fast_trigrams_basic() {
        let mut trigrams = extract_trigrams_ascii_fast("hello");
        trigrams.sort();
        assert_eq!(trigrams, vec!["ell", "hel", "llo"]);
    }

    #[test]
    fn test_ascii_fast_trigrams_short() {
        assert_eq!(extract_trigrams_ascii_fast("ab"), vec!["ab"]);
        assert_eq!(extract_trigrams_ascii_fast(""), vec![""]);
    }

    #[test]
    fn test_ascii_fast_trigrams_single() {
        assert_eq!(extract_trigrams_ascii_fast("abc"), vec!["abc"]);
    }

    #[test]
    fn test_ascii_fast_trigrams_dedup() {
        assert_eq!(extract_trigrams_ascii_fast("aaaa"), vec!["aaa"]);
    }

    #[test]
    fn test_ascii_fast_trigrams_long() {
        let trigrams = extract_trigrams_ascii_fast("abcdefghijklmnop");
        // 16 chars → 14 unique trigrams
        assert_eq!(trigrams.len(), 14);
        assert_eq!(trigrams[0], "abc");
        assert_eq!(trigrams[13], "nop");
    }

    #[test]
    fn test_ascii_fast_trigrams_realistic_symbol() {
        let trigrams = extract_trigrams_ascii_fast("createCompilerHost");
        // 18 chars → 16 unique trigrams
        assert_eq!(trigrams.len(), 16);
        assert!(trigrams.contains(&"cre".to_string()));
        assert!(trigrams.contains(&"ost".to_string()));
    }

    #[test]
    fn test_ascii_fast_trigrams_matches_scalar() {
        // Verify the ASCII fast path produces the same results as scalar
        use super::super::scalar;
        let inputs = [
            "hello",
            "abc",
            "abcdefghijklmnopqrstuvwxyz",
            "createCompilerHost",
            "aaaaaa",
            "HELLO_WORLD_123",
        ];
        for input in &inputs {
            let mut fast = extract_trigrams_ascii_fast(input);
            let mut scalar = scalar::extract_trigrams(input);
            fast.sort();
            scalar.sort();
            assert_eq!(fast, scalar, "mismatch for input: {input}");
        }
    }
}
