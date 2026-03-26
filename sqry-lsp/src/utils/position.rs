//! UTF-16 ↔ byte position translation utilities for LSP
//!
//! LSP uses UTF-16 code units for positions; Rust strings are UTF-8.
//! These helpers convert between UTF-16 code-unit indices and UTF-8 byte offsets.

/// Convert a UTF-16 code unit index into a byte offset within `s`.
///
/// If the index points beyond the end, returns `s.len()`.
#[must_use]
pub fn utf16_to_byte(s: &str, mut utf16_index: usize) -> usize {
    // Return byte offset AFTER consuming `utf16_index` UTF-16 code units
    // (i.e., caret position semantics used by LSP).
    if utf16_index == 0 {
        return 0;
    }
    let mut byte = 0;
    for ch in s.chars() {
        let u16_len = ch.len_utf16();
        if utf16_index <= u16_len {
            // We consume the remainder within this char boundary.
            byte += ch.len_utf8();
            return byte;
        }
        utf16_index -= u16_len;
        byte += ch.len_utf8();
    }
    s.len()
}

/// Convert a byte offset (on a character boundary) to a UTF-16 code-unit index.
///
/// If the byte index is not on a char boundary, it is rounded down to the previous boundary.
#[must_use]
pub fn byte_to_utf16(s: &str, byte_index: usize) -> usize {
    let mut idx = 0usize;
    let mut bytes = 0usize;
    for ch in s.chars() {
        if bytes >= byte_index {
            break;
        }
        idx += ch.len_utf16();
        bytes += ch.len_utf8();
    }
    idx
}

/// Get the byte offset within a specific line given (`line_text`, `utf16_col`).
#[must_use]
pub fn line_utf16_col_to_byte(line_text: &str, utf16_col: usize) -> usize {
    utf16_to_byte(line_text, utf16_col)
}

/// Get the UTF-16 column within a specific line given (`line_text`, `byte_col`).
#[must_use]
pub fn line_byte_to_utf16_col(line_text: &str, byte_col: usize) -> usize {
    byte_to_utf16(line_text, byte_col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_roundtrip() {
        let s = "hello"; // 5 bytes, 5 utf16 units
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1);
        assert_eq!(utf16_to_byte(s, 5), 5);
        assert_eq!(byte_to_utf16(s, 0), 0);
        assert_eq!(byte_to_utf16(s, 4), 4);
        assert_eq!(byte_to_utf16(s, 5), 5);
    }

    #[test]
    fn latin1_roundtrip() {
        let s = "héllo"; // 'é' U+00E9: utf8=2 bytes, utf16=1 unit
        // bytes: [h][é=2][l][l][o]
        assert_eq!(utf16_to_byte(s, 1), 1); // after 'h'
        assert_eq!(utf16_to_byte(s, 2), 1 + 2); // after 'é'
        assert_eq!(byte_to_utf16(s, 1), 1);
        assert_eq!(byte_to_utf16(s, s.len()), 5);
    }

    #[test]
    fn emoji_surrogate_pair() {
        let s = "a🙂b"; // U+1F642: utf8=4 bytes, utf16=2 units
        // chars: 'a'(1u16), '🙂'(2u16), 'b'(1u16)
        assert_eq!(utf16_to_byte(s, 1), 1); // after 'a'
        assert_eq!(utf16_to_byte(s, 2), 1 + 4); // after first unit of '🙂' => end of emoji
        assert_eq!(utf16_to_byte(s, 4), 1 + 4 + 1); // after 'b'
        assert_eq!(byte_to_utf16(s, 1), 1);
        assert_eq!(byte_to_utf16(s, 1 + 4), 3);
        assert_eq!(byte_to_utf16(s, s.len()), 4);
    }

    #[test]
    fn combining_mark() {
        let s = "e\u{0301}"; // 'e' + COMBINING ACUTE (two code points), utf16 units: 1 + 1
        // utf8 bytes: 1 + 2
        assert_eq!(utf16_to_byte(s, 1), 1); // after 'e'
        assert_eq!(utf16_to_byte(s, 2), 1 + 2); // after combining mark
        assert_eq!(byte_to_utf16(s, 1), 1);
        assert_eq!(byte_to_utf16(s, s.len()), 2);
    }
}
