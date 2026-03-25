//! UTF-8 malformed input generators.
//!
//! These generators create various types of malformed UTF-8 sequences
//! to test tree-sitter's handling of invalid input.

/// Generates input with truncated UTF-8 multi-byte sequences.
///
/// Creates valid UTF-8 text with truncated sequences at the end,
/// simulating incomplete reads or corrupted files.
///
/// # Returns
/// Byte vector containing valid UTF-8 followed by a truncated 3-byte sequence.
#[must_use]
pub fn generate_truncated_utf8() -> Vec<u8> {
    let mut result = Vec::new();

    // Valid UTF-8 prefix
    result.extend_from_slice(b"fn main() { ");

    // Start of a 3-byte UTF-8 sequence for Euro sign (U+20AC: E2 82 AC)
    // but truncate it to just the first 2 bytes
    result.push(0xE2); // Start of 3-byte sequence
    result.push(0x82); // First continuation byte
                       // Missing final byte 0xAC - TRUNCATED

    result
}

/// Generates input with invalid UTF-8 continuation bytes.
///
/// Creates sequences that start a multi-byte character but have
/// invalid continuation bytes.
///
/// # Returns
/// Byte vector with invalid continuation bytes.
#[must_use]
pub fn generate_invalid_continuation() -> Vec<u8> {
    let mut result = Vec::new();

    // Valid UTF-8 prefix
    result.extend_from_slice(b"class Main { ");

    // Start a 3-byte sequence but use invalid continuation bytes
    result.push(0xE2); // Start of 3-byte sequence
    result.push(0xFF); // INVALID continuation byte (should be 0x80-0xBF)
    result.push(0xFF); // INVALID continuation byte

    result
}

/// Generates input with overlong UTF-8 encodings.
///
/// Creates sequences that encode characters using more bytes than necessary,
/// which is invalid per UTF-8 spec (security risk: can bypass filters).
///
/// # Returns
/// Byte vector with overlong encodings.
#[must_use]
pub fn generate_overlong_encoding() -> Vec<u8> {
    let mut result = Vec::new();

    // Valid UTF-8 prefix
    result.extend_from_slice(b"SELECT * FROM ");

    // Overlong encoding of '/' (U+002F, should be 1 byte: 0x2F)
    // Encoded as 2-byte sequence: C0 AF (INVALID - should be rejected)
    result.push(0xC0);
    result.push(0xAF);

    // Another overlong: NUL (U+0000, should be 1 byte: 0x00)
    // Encoded as 2-byte: C0 80 (INVALID - security risk)
    result.push(0xC0);
    result.push(0x80);

    result
}

/// Generates input with UTF-8 surrogate pair code points.
///
/// UTF-16 surrogate pairs (U+D800 to U+DFFF) are invalid in UTF-8.
///
/// # Returns
/// Byte vector with invalid surrogate pair encodings.
#[must_use]
pub fn generate_surrogate_pairs() -> Vec<u8> {
    let mut result = Vec::new();

    // Valid UTF-8 prefix
    result.extend_from_slice(b"<div>");

    // UTF-16 high surrogate U+D800 encoded in UTF-8 (INVALID)
    // Would be: ED A0 80 (but this is illegal in UTF-8)
    result.push(0xED);
    result.push(0xA0);
    result.push(0x80);

    // UTF-16 low surrogate U+DFFF encoded in UTF-8 (INVALID)
    // Would be: ED BF BF (but this is illegal in UTF-8)
    result.push(0xED);
    result.push(0xBF);
    result.push(0xBF);

    result.extend_from_slice(b"</div>");

    result
}

/// Generates input with embedded null bytes in otherwise valid UTF-8.
///
/// Tests handling of NUL bytes (0x00) which can cause issues with
/// C-string-based parsers.
///
/// # Returns
/// Byte vector with embedded null bytes.
#[must_use]
pub fn generate_null_bytes() -> Vec<u8> {
    let mut result = Vec::new();

    // Valid UTF-8 with embedded NUL
    result.extend_from_slice(b"def ");
    result.push(0x00); // NUL byte
    result.extend_from_slice(b"main");
    result.push(0x00); // NUL byte
    result.extend_from_slice(b"():");

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncated_utf8() {
        let result = generate_truncated_utf8();

        // Should not be valid UTF-8
        assert!(std::str::from_utf8(&result).is_err());

        // Should have the truncated sequence at the end
        assert_eq!(result[result.len() - 2], 0xE2);
        assert_eq!(result[result.len() - 1], 0x82);
    }

    #[test]
    fn test_invalid_continuation() {
        let result = generate_invalid_continuation();

        // Should not be valid UTF-8
        assert!(std::str::from_utf8(&result).is_err());

        // Should contain invalid continuation bytes
        assert!(result.contains(&0xFF));
    }

    #[test]
    fn test_overlong_encoding() {
        let result = generate_overlong_encoding();

        // Should not be valid UTF-8
        assert!(std::str::from_utf8(&result).is_err());

        // Should contain overlong sequences
        let has_overlong = result
            .windows(2)
            .any(|w| w == [0xC0, 0xAF] || w == [0xC0, 0x80]);
        assert!(has_overlong);
    }

    #[test]
    fn test_surrogate_pairs() {
        let result = generate_surrogate_pairs();

        // Should not be valid UTF-8 (surrogate pairs are invalid)
        assert!(std::str::from_utf8(&result).is_err());

        // Should contain surrogate sequences
        let has_surrogate = result
            .windows(3)
            .any(|w| w == [0xED, 0xA0, 0x80] || w == [0xED, 0xBF, 0xBF]);
        assert!(has_surrogate);
    }

    #[test]
    fn test_null_bytes() {
        let result = generate_null_bytes();

        // Contains null bytes but otherwise valid UTF-8
        assert!(result.contains(&0x00));

        // Should have def, main, and () in it
        assert!(result.windows(3).any(|w| w == b"def"));
        assert!(result.windows(4).any(|w| w == b"main"));
    }

    #[test]
    fn test_all_generators_produce_output() {
        assert!(!generate_truncated_utf8().is_empty());
        assert!(!generate_invalid_continuation().is_empty());
        assert!(!generate_overlong_encoding().is_empty());
        assert!(!generate_surrogate_pairs().is_empty());
        assert!(!generate_null_bytes().is_empty());
    }
}
