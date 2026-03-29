#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_core::query::parser_new::Parser;

fuzz_target!(|data: &[u8]| {
    // Convert arbitrary bytes to UTF-8 string
    if let Ok(query_str) = std::str::from_utf8(data) {
        // Fuzz the parser
        // We don't care if parsing succeeds or fails, only that it doesn't panic
        let _ = Parser::parse_query(query_str);
    }
});
