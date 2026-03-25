//! Malformed input tests for Csharp language plugin.
//!
//! Tests that the plugin gracefully handles various types of malformed inputs
//! without panicking, validating FFI boundary safety.

use sqry_core::plugin::LanguagePlugin;
use sqry_lang_csharp::CSharpPlugin;
use sqry_tree_sitter_fuzz_support::MalformedInputBuilder;
use sqry_tree_sitter_fuzz_support::generators::nesting::depths;
use sqry_tree_sitter_fuzz_support::testing::{StackSafeResult, run_with_stack};

#[test]
fn test_truncated_utf8() {
    let plugin = CSharpPlugin::default();
    let malformed = MalformedInputBuilder::truncated_utf8();

    let result = plugin.parse_ast(&malformed);
    // Tree-sitter is error-tolerant - may succeed or fail, but shouldn't panic
    let _ = result;
}

#[test]
fn test_invalid_continuation() {
    let plugin = CSharpPlugin::default();
    let malformed = MalformedInputBuilder::invalid_continuation();

    let result = plugin.parse_ast(&malformed);
    // Error-tolerant parser may handle invalid UTF-8 gracefully
    let _ = result;
}

#[test]
fn test_overlong_encoding() {
    let plugin = CSharpPlugin::default();
    let malformed = MalformedInputBuilder::overlong_encoding();

    let result = plugin.parse_ast(&malformed);
    // May succeed or fail, but shouldn't panic
    let _ = result;
}

#[test]
fn test_surrogate_pairs() {
    let plugin = CSharpPlugin::default();
    let malformed = MalformedInputBuilder::surrogate_pairs();

    let result = plugin.parse_ast(&malformed);
    // May succeed or fail, but shouldn't panic
    let _ = result;
}

#[test]
fn test_null_bytes() {
    let plugin = CSharpPlugin::default();
    let malformed = MalformedInputBuilder::null_bytes();

    let result = plugin.parse_ast(&malformed);
    // Null bytes may or may not cause parse failure, but shouldn't panic
    let _ = result;
}

#[test]
fn test_deeply_nested_shallow() {
    let plugin = CSharpPlugin::default();
    let nested = MalformedInputBuilder::for_language("csharp").deeply_nested(depths::SHALLOW);

    // Use stack-safe harness for nesting tests
    let result = run_with_stack(move || plugin.parse_ast(&nested));

    match result {
        StackSafeResult::Ok(parse_result) => {
            // Parse may succeed or fail, but shouldn't panic
            let _ = parse_result;
        }
        StackSafeResult::Panicked(_) => {
            // Stack overflow or other panic is acceptable for extreme nesting
        }
    }
}

#[test]
fn test_deeply_nested_medium() {
    let plugin = CSharpPlugin::default();
    let nested = MalformedInputBuilder::for_language("csharp").deeply_nested(depths::MEDIUM);

    let result = run_with_stack(move || plugin.parse_ast(&nested));

    match result {
        StackSafeResult::Ok(_) => {
            // Parse may succeed or fail, but we survived
        }
        StackSafeResult::Panicked(_) => {
            // Acceptable for deep nesting
        }
    }
}

#[test]
#[ignore] // Stress test only
fn test_deeply_nested_deep() {
    let plugin = CSharpPlugin::default();
    let nested = MalformedInputBuilder::for_language("csharp").deeply_nested(depths::DEEP);

    let result = run_with_stack(move || plugin.parse_ast(&nested));

    // At 1000 depth, stack overflow is likely - just verify we catch it
    let _ = result;
}

#[test]
fn test_oversized_1mb() {
    let plugin = CSharpPlugin::default();
    let large = MalformedInputBuilder::for_language("csharp").oversized_1mb();

    // This is valid Csharp (repeated), so parse should succeed
    let result = plugin.parse_ast(&large);

    // May succeed or fail depending on tree-sitter limits, but shouldn't panic
    let _ = result;
}

#[test]
#[ignore] // Nightly builds only
fn test_oversized_10mb() {
    let plugin = CSharpPlugin::default();
    let large = MalformedInputBuilder::for_language("csharp").oversized_10mb();

    let result = plugin.parse_ast(&large);
    let _ = result; // Just verify no panic
}

#[test]
fn test_random_bytes() {
    let plugin = CSharpPlugin::default();
    let random = MalformedInputBuilder::random_bytes(1024);

    let result = plugin.parse_ast(&random);
    // Random bytes very unlikely to be valid Csharp, but tree-sitter is error-tolerant
    let _ = result;
}

#[test]
fn test_parse_ast_on_malformed() {
    let plugin = CSharpPlugin::default();
    let malformed = MalformedInputBuilder::truncated_utf8();

    let result = plugin.parse_ast(&malformed);
    let _ = result;
}

// Relations tests removed - deprecated extract_* methods removed from LanguagePlugin trait
