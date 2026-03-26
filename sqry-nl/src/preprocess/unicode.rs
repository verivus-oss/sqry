//! Unicode normalization utilities.

use unicode_normalization::UnicodeNormalization;

/// Apply NFKC normalization to input string.
///
/// NFKC (Normalization Form Compatibility Composition) handles:
/// - Decomposition of compatibility characters
/// - Canonical recomposition
/// - Full-width → half-width character conversion
#[must_use]
pub fn normalize_nfkc(input: &str) -> String {
    input.nfkc().collect()
}

/// Strip zero-width and invisible characters.
///
/// Removes:
/// - U+200B Zero-width space
/// - U+200C Zero-width non-joiner
/// - U+200D Zero-width joiner
/// - U+FEFF Byte order mark
/// - U+00AD Soft hyphen
/// - U+2060 Word joiner
/// - U+180E Mongolian vowel separator
#[must_use]
pub fn strip_zero_width(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{200B}' // Zero-width space
                | '\u{200C}' // Zero-width non-joiner
                | '\u{200D}' // Zero-width joiner
                | '\u{FEFF}' // BOM
                | '\u{00AD}' // Soft hyphen
                | '\u{2060}' // Word joiner
                | '\u{180E}' // Mongolian vowel separator
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nfkc_basic() {
        // Full-width to half-width
        assert_eq!(normalize_nfkc("ｆｉｎｄ"), "find");
    }

    #[test]
    fn test_nfkc_composed() {
        // Composed characters remain composed
        let input = "café";
        let normalized = normalize_nfkc(input);
        assert!(normalized.contains('é') || normalized.contains("cafe"));
    }

    #[test]
    fn test_strip_zero_width() {
        let input = "find\u{200B}foo";
        assert_eq!(strip_zero_width(input), "findfoo");
    }

    #[test]
    fn test_strip_multiple_invisible() {
        let input = "\u{FEFF}find\u{200D}foo\u{00AD}bar";
        assert_eq!(strip_zero_width(input), "findfoobar");
    }

    #[test]
    fn test_no_change_normal_input() {
        let input = "find authentication";
        assert_eq!(strip_zero_width(input), input);
    }
}
