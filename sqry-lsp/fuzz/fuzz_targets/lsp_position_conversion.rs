#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_lsp::utils::position::{byte_to_utf16, line_byte_to_utf16_col, line_utf16_col_to_byte, utf16_to_byte};

fuzz_target!(|data: &[u8]| {
    // Fuzz UTF-16 ↔ byte position conversion functions
    // These are critical for correct LSP position handling with Unicode text

    if let Ok(text) = std::str::from_utf8(data) {
        // Test with various UTF-16 indices
        for utf16_idx in 0..text.len().saturating_add(10) {
            let byte_offset = utf16_to_byte(text, utf16_idx);
            // byte_offset should not exceed text length
            assert!(byte_offset <= text.len());
        }

        // Test byte to UTF-16 conversion
        for byte_idx in 0..=text.len() {
            let utf16_idx = byte_to_utf16(text, byte_idx);
            // UTF-16 index should not be absurdly large
            assert!(utf16_idx <= text.len() * 2); // Each char is at most 2 UTF-16 units
        }

        // Test line-specific conversions
        for line in text.lines() {
            for col in 0..line.len().saturating_add(5) {
                let byte_col = line_utf16_col_to_byte(line, col);
                assert!(byte_col <= line.len());

                let utf16_col = line_byte_to_utf16_col(line, col.min(line.len()));
                assert!(utf16_col <= line.len() * 2);
            }
        }
    }
});
