#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_mcp::{decode_cursor, encode_cursor};

fuzz_target!(|data: &[u8]| {
    // Fuzz cursor decoding with arbitrary bytes
    // We don't care if decoding succeeds or fails, only that it doesn't panic

    if let Ok(cursor_str) = std::str::from_utf8(data) {
        // Test decoding arbitrary cursor strings
        let _ = decode_cursor(cursor_str);
    }

    // Also test the roundtrip: encode a value derived from bytes, then decode
    if data.len() >= 8 {
        let offset = usize::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);
        // Clamp to reasonable range to avoid memory issues
        let clamped_offset = offset % 1_000_000;
        let encoded = encode_cursor(clamped_offset);
        let decoded = decode_cursor(&encoded);
        // Roundtrip should succeed
        assert!(decoded.is_ok());
        assert_eq!(decoded.unwrap(), clamped_offset);
    }
});
