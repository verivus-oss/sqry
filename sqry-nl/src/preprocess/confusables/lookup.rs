//! Binary search lookup for Unicode confusable characters.
//!
//! Provides O(log n) lookup of confusable character mappings using sorted
//! parallel arrays from the auto-generated [`data`](super::data) module.

use super::data::{SOURCE_CODEPOINTS, TARGET_STRINGS};

/// Look up the replacement string for a confusable character.
///
/// Returns `Some(replacement)` if `c` is a known confusable, `None` otherwise.
/// Uses binary search on the sorted `SOURCE_CODEPOINTS` array (~13 comparisons
/// for 6,311 entries).
#[inline]
pub fn lookup(c: char) -> Option<&'static str> {
    SOURCE_CODEPOINTS
        .binary_search(&c)
        .ok()
        .map(|i| TARGET_STRINGS[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_known_confusable_mapping() {
        // Cyrillic а (U+0430) → "a"
        assert_eq!(lookup('\u{0430}'), Some("a"));
    }

    #[test]
    fn test_ascii_char_in_table() {
        // m (U+006D) → "rn" — proves table contains ASCII sources
        assert_eq!(lookup('m'), Some("rn"));
    }

    #[test]
    fn test_non_confusable_non_ascii() {
        // Snowman (U+2603) has no confusable mapping
        assert_eq!(lookup('\u{2603}'), None);
    }

    #[test]
    fn test_quote_mapping() {
        // " (U+0022) → '' per TR39 (matches homoglyph.rs:88 comment)
        assert_eq!(lookup('"'), Some("''"));
    }

    #[test]
    fn test_m_to_rn_in_table() {
        // m → rn is in table (validates why ASCII guard exists)
        let result = lookup('m');
        assert_eq!(result, Some("rn"));
    }
}
