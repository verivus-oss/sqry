#![no_main]
//! Fuzz target for query parser pipeline
//!
//! Exercises the boolean AST parser with arbitrary input to ensure it never panics.

use libfuzzer_sys::fuzz_target;
use sqry_core::query::QueryParser;

fuzz_target!(|data: &[u8]| {
    // Convert arbitrary bytes to UTF-8 string
    if let Ok(query_str) = std::str::from_utf8(data) {
        // Fuzz the boolean parser
        // We don't care if parsing succeeds or fails, only that it doesn't panic
        let _ = QueryParser::parse_query(query_str);
    }
});
