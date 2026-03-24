//! Tests to verify regex validator security
//!
//! NOTE: Consecutive quantifier validation has been REMOVED as of Sprint 1 Phase 1.
//! Rust's `regex` crate uses Thompson NFA/DFA construction and is immune to catastrophic
//! backtracking. Patterns like .*.+, \w*\w*, [a-z]+\d* are safe and will not cause `ReDoS`.
//!
//! We still check nested quantifiers (e.g., (a+)+) for code clarity, as they can indicate
//! logic errors, even though they don't cause performance issues.
//!
//! See `CRGREP_RUST_COMPARISON.md` and `PHASE1_CODEX_REVIEW_RESPONSE.md` for detailed rationale.

use sqry_core::ast::parse_query;

use sqry_core::test_support::verbosity;
use std::sync::Once;

// Initialize verbose logging once for all tests in this file
static INIT: Once = Once::new();

fn init_logging() {
    INIT.call_once(|| {
        verbosity::init(env!("CARGO_PKG_NAME"));
    });
}

#[test]
fn test_consecutive_quantifiers_allowed() {
    init_logging();
    log::info!("Testing test_consecutive_quantifiers_allowed");

    println!("\n=== Verifying Consecutive Quantifiers Are Allowed ===\n");
    println!("NOTE: Rust's regex crate is immune to catastrophic backtracking.\n");

    // These patterns are SAFE in Rust and should be ALLOWED
    let safe_patterns = vec![
        (".*.*", "consecutive wildcards - safe"),
        (".+.+", "consecutive plus wildcards - safe"),
        ("\\w*\\w*", "consecutive word quantifiers - safe"),
        ("\\d+\\d+", "consecutive digit quantifiers - safe"),
        ("[a-z]+\\d*", "disjoint character classes - safe"),
        ("\\w+\\s*", "word followed by whitespace - safe"),
    ];

    for (pattern, desc) in safe_patterns {
        println!("Testing: {pattern} ({desc})");
        let result = parse_query(&format!("name~={pattern}"));
        match result {
            Ok(_) => println!("  ✓ ALLOWED (correct - Rust regex is safe)"),
            Err(e) => panic!("FALSE POSITIVE: Safe pattern '{pattern}' was blocked: {e}"),
        }
    }

    println!("\nAll consecutive quantifier patterns are allowed (as expected).");
    println!("Rust's regex crate guarantees O(n) matching time.");
}

#[test]
fn test_nested_quantifiers_detection() {
    println!("\n=== Testing Nested Quantifiers ===\n");

    let dangerous_patterns = vec![
        ("(a+)+", "nested quantifiers"),
        ("(x*)*", "nested star quantifiers"),
        ("(\\w+)+", "nested word quantifiers"),
    ];

    for (pattern, desc) in dangerous_patterns {
        println!("Testing: {pattern} ({desc})");

        let result = parse_query(&format!("name~={pattern}"));

        match result {
            Ok(_) => {
                println!("  ⚠️  VULNERABILITY: Pattern '{pattern}' was ALLOWED");
                println!("      This can cause exponential backtracking!");
            }
            Err(e) => {
                println!("  ✓ BLOCKED: {e}");
            }
        }
    }

    // Validator SHOULD block (x+)+ according to code (lines 107-114)
    let result_nested = parse_query("name~=(a+)+");
    assert!(
        result_nested.is_err(),
        "Pattern '(a+)+' should be blocked (nested quantifiers)"
    );
}

#[test]
fn test_lookahead_quantifiers() {
    println!("\n=== Testing Lookahead/Lookbehind Quantifiers (M1 from review) ===\n");

    // M1: Patterns with quantified lookaheads can be expensive
    let patterns = vec![
        ("(?=a+)+", "quantified lookahead"),
        ("(?!x*)*", "quantified negative lookahead"),
    ];

    for (pattern, desc) in patterns {
        println!("Testing: {pattern} ({desc})");

        let result = parse_query(&format!("name~={pattern}"));

        match result {
            Ok(_) => {
                println!("  ⚠️  MEDIUM RISK: Pattern '{pattern}' was ALLOWED");
                println!("      Lookaheads with quantifiers can be expensive");
            }
            Err(e) => {
                println!("  ✓ BLOCKED: {e}");
            }
        }
    }

    // Review says this is MEDIUM severity and currently not detected
    // Let's verify
    let result = parse_query("name~=(?=a+)+");
    if result.is_ok() {
        println!("\n⚠️  CONFIRMED: Lookahead quantifiers not currently detected");
        println!("   Severity: MEDIUM (per review)");
        println!("   Recommendation: Document as known limitation or add detection");
    }
}

#[test]
fn test_safe_patterns_allowed() {
    println!("\n=== Testing Safe Patterns (Should Be Allowed) ===\n");

    let safe_patterns = vec![
        (".*", "single wildcard quantifier"),
        ("a+", "simple quantifier"),
        ("\\w+", "word quantifier"),
        ("(a|b)", "alternation without quantifier"),
        ("a{1,10}", "bounded repetition"),
    ];

    for (pattern, desc) in safe_patterns {
        println!("Testing: {pattern} ({desc})");

        let result = parse_query(&format!("name~={pattern}"));

        match result {
            Ok(_) => {
                println!("  ✓ ALLOWED (correct)");
            }
            Err(e) => {
                println!("  ❌ BLOCKED (should be allowed): {e}");
            }
        }
    }

    // Safe patterns should be allowed
    let result = parse_query("name~=.*");
    assert!(result.is_ok(), "Safe pattern '.*' should be allowed");
}

// NOTE: Consecutive quantifier validation has been removed.
//
// Rust's `regex` crate uses Thompson NFA/DFA construction and is immune to
// catastrophic backtracking. Patterns like .*.+, \w*\w*, or [a-z]+\d* are safe.
//
// This test has been removed. See CRGREP_RUST_COMPARISON.md for rationale.

#[test]
fn test_deep_alternation_patterns() {
    println!("\n=== Testing Deep Alternation Patterns ===\n");

    // Deep nesting without quantifiers — should be ALLOWED
    let safe_nested = vec![
        ("(a|(b|c))", "nested alternation depth 2"),
        ("((foo|bar)|baz)", "nested alternation depth 2 reversed"),
        ("(a|(b|(c|d)))", "nested alternation depth 3"),
    ];

    for (pattern, desc) in safe_nested {
        println!("Testing: {pattern} ({desc})");
        let result = parse_query(&format!("name~={pattern}"));
        match result {
            Ok(_) => println!("  ✓ ALLOWED (correct)"),
            Err(e) => {
                println!("  ❌ BLOCKED (should be allowed): {e}");
                panic!("FALSE POSITIVE: Nested alternation '{pattern}' should be allowed");
            }
        }
    }
}

#[test]
fn test_boolean_grouping_with_regex_alternation() {
    println!("\n=== Testing Boolean Grouping vs Regex Alternation ===\n");

    // Boolean grouping with parentheses — should work
    let boolean_queries = vec![
        "(kind:function AND name~=foo)",
        "(kind:method OR kind:function)",
        "((kind:function AND name~=foo) OR name~=bar)",
    ];

    for query in boolean_queries {
        println!("Testing boolean: {query}");
        let result = parse_query(query);
        match result {
            Ok(_) => println!("  ✓ Boolean grouping works"),
            Err(e) => {
                println!("  ❌ Boolean grouping broken: {e}");
                panic!("REGRESSION: Boolean grouping '{query}' should work");
            }
        }
    }

    // Regex alternation in name predicate — should work
    println!("\nTesting regex alternation: name~=(foo|bar)");
    let result = parse_query("name~=(foo|bar)");
    match result {
        Ok(_) => println!("  ✓ Regex alternation works"),
        Err(e) => panic!("REGRESSION: Regex alternation should work: {e}"),
    }
}
