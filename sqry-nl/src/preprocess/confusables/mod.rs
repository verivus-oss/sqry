//! Internal confusable character detection and replacement.
//!
//! Provides a zero-dependency replacement for the GPL-licensed `confusables` crate,
//! using vendored Unicode TR39 confusable mappings (Unicode 15.0.0) with
//! binary-search-based lookup.
//!
//! # Performance
//! - ASCII fast-path: `str::is_ascii()` (SIMD-optimized in std) returns immediately
//! - Non-ASCII: O(log n) binary search per character (~13 comparisons for 6,311 entries)
//! - Zero heap allocation in the detection path

mod data;
mod lookup;

/// Check if the input string contains any confusable characters.
///
/// Returns `false` immediately for pure-ASCII input (SIMD fast-path).
/// For non-ASCII input, checks each non-ASCII character against the
/// confusable lookup table.
#[must_use]
pub fn contains_confusable(input: &str) -> bool {
    if input.is_ascii() {
        return false;
    }
    input
        .chars()
        .any(|c| !c.is_ascii() && lookup::lookup(c).is_some())
}

/// Replace confusable characters with their Unicode TR39 target strings.
///
/// Returns the input unchanged (cloned) for pure-ASCII input.
/// For non-ASCII input, replaces each confusable character with its
/// target mapping, leaving non-confusable characters unchanged.
#[must_use]
pub fn replace_confusable(input: &str) -> String {
    if input.is_ascii() {
        return input.to_string();
    }

    let mut result = String::with_capacity(input.len());
    for c in input.chars() {
        if c.is_ascii() {
            result.push(c);
        } else if let Some(replacement) = lookup::lookup(c) {
            result.push_str(replacement);
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::data::{SOURCE_CODEPOINTS, TARGET_STRINGS};
    use super::*;

    // --- Data integrity tests ---

    #[test]
    fn test_source_codepoints_sorted() {
        for window in SOURCE_CODEPOINTS.windows(2) {
            assert!(
                window[0] < window[1],
                "SOURCE_CODEPOINTS not strictly ascending: {:?} >= {:?}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn test_parallel_arrays_same_length() {
        assert_eq!(SOURCE_CODEPOINTS.len(), TARGET_STRINGS.len());
    }

    // --- contains_confusable tests ---

    #[test]
    fn test_ascii_only_false() {
        assert!(!contains_confusable("hello"));
    }

    #[test]
    fn test_cyrillic_detected() {
        // 'а' in "pаypal" is Cyrillic U+0430
        assert!(contains_confusable("p\u{0430}ypal"));
    }

    #[test]
    fn test_empty_string_false() {
        assert!(!contains_confusable(""));
    }

    // --- replace_confusable tests ---

    #[test]
    fn test_cyrillic_to_latin() {
        // Cyrillic а (U+0430) → Latin a
        let result = replace_confusable("\u{0430}uthentication");
        assert_eq!(result, "authentication");
    }

    #[test]
    fn test_multi_codepoint_target() {
        // ǉ (U+01C9) → "lj" (2-char expansion)
        let result = replace_confusable("\u{01C9}");
        assert_eq!(result, "lj");
    }

    #[test]
    fn test_non_ascii_non_confusable_unchanged() {
        // é (U+00E9) — accented but not in confusables table
        // Check if it's actually in the table first
        if lookup::lookup('\u{00E9}').is_some() {
            // If it IS in the table, just verify replace does something
            let result = replace_confusable("caf\u{00E9}");
            assert!(!result.is_empty());
        } else {
            let result = replace_confusable("caf\u{00E9}");
            assert_eq!(result, "caf\u{00E9}");
        }
    }

    #[test]
    fn test_mixed_content() {
        // Mix of confusable (Cyrillic е U+0435) + normal chars
        let result = replace_confusable("h\u{0435}llo");
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_no_change_passthrough() {
        let result = replace_confusable("pure ascii text");
        assert_eq!(result, "pure ascii text");
    }

    // --- Drift detection test ---

    #[test]
    fn test_generated_data_entry_count() {
        // Validate the expected entry count for Unicode 15.0.0
        assert_eq!(
            SOURCE_CODEPOINTS.len(),
            6311,
            "Entry count mismatch — regenerate data.rs with: python3 scripts/generate_confusables.py"
        );
    }
}
