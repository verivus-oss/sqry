//! Homoglyph detection and normalization.
//!
//! Converts visually similar characters (confusables) to their ASCII equivalents
//! to prevent homoglyph attacks.
//!
//! NOTE: The Unicode TR39 skeleton transformation maps ASCII 'm' → 'rn'
//! (because they look similar). This corrupts normal text.
//! We only apply confusable normalization when non-ASCII characters are detected.

use super::confusables;

/// Check if string contains any non-ASCII characters that could be homoglyphs.
fn has_non_ascii(input: &str) -> bool {
    !input.is_ascii()
}

/// Replace confusable characters with their ASCII equivalents.
///
/// Only processes input that contains non-ASCII characters to avoid
/// corrupting normal ASCII text (Unicode TR39 skeleton maps 'm' → 'rn').
///
/// Returns the normalized string and whether any replacements were made.
#[must_use]
pub fn replace_confusables(input: &str) -> (String, bool) {
    // Only apply confusables normalization if non-ASCII characters are present
    // The TR39 skeleton transformation corrupts pure ASCII
    // (e.g., 'm' → 'rn') which we want to avoid
    if !has_non_ascii(input) {
        return (input.to_string(), false);
    }

    // Check if the input contains actual confusables (not just any non-ASCII)
    if confusables::contains_confusable(input) {
        (confusables::replace_confusable(input), true)
    } else {
        (input.to_string(), false)
    }
}

/// Check if input contains any confusable characters.
///
/// Returns false for pure ASCII input (no homoglyph risk).
#[must_use]
#[allow(dead_code)] // May be used in future security checks
pub fn contains_confusables(input: &str) -> bool {
    has_non_ascii(input) && confusables::contains_confusable(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cyrillic_a() {
        // Cyrillic 'а' (U+0430) looks like Latin 'a'
        let input = "\u{0430}uthentication"; // First char is Cyrillic
        let (normalized, changed) = replace_confusables(input);
        assert!(changed);
        assert_eq!(normalized, "authentication");
    }

    #[test]
    fn test_no_confusables() {
        let input = "authentication";
        let (normalized, changed) = replace_confusables(input);
        assert!(!changed);
        assert_eq!(normalized, input);
    }

    #[test]
    fn test_contains_confusables() {
        let input_with = "\u{0430}uthentication"; // Cyrillic 'а'
        let input_without = "authentication";

        assert!(contains_confusables(input_with));
        assert!(!contains_confusables(input_without));
    }

    #[test]
    fn test_mixed_scripts() {
        // Mix of Latin and Cyrillic that looks like "hello"
        let input = "h\u{0435}llo"; // 'е' is Cyrillic
        let (normalized, changed) = replace_confusables(input);
        assert!(changed);
        assert_eq!(normalized, "hello");
    }

    // Note: The confusables data correctly maps ASCII " to '' per Unicode TR39.
    // We handle this at the preprocessing level by extracting quotes BEFORE
    // homoglyph normalization, so quotes are never passed to this function.
}
