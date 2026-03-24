//! Test support utilities for sqry tree-sitter integration
//!
//! This crate provides a safe, minimal dummy grammar for testing purposes.
//! It is designed to replace unsafe `Language::from_raw(std::ptr::null())` usages
//! throughout the sqry codebase with a valid, tested Language handle.
//!
//! # Safety Invariants
//!
//! The test grammar provided by this crate guarantees:
//! 1. **Non-null pointer**: Never returns null Language handles
//! 2. **Valid ABI version**: Uses current tree-sitter `LANGUAGE_VERSION`
//! 3. **Minimal but valid grammar**: Has exactly 1 symbol (enough to pass validation)
//! 4. **Static lifetime**: Grammar lives for 'static, safe to use anywhere
//! 5. **No external dependencies**: Self-contained, no C compilation required
//!
//! # Usage
//!
//! ```rust
//! use sqry_test_support::test_language;
//! use tree_sitter::Language;
//!
//! // Replace this unsafe pattern:
//! // let lang = unsafe { Language::from_raw(std::ptr::null()) };
//!
//! // With this safe helper:
//! let lang = test_language();
//!
//! // The language can be used in tests but should NOT be used for actual parsing
//! assert_eq!(lang.node_kind_count(), 1); // Minimal valid grammar
//! ```
//!
//! # Warning
//!
//! This test grammar is **NOT suitable for actual parsing**. It is designed solely
//! for testing code paths that require a valid Language handle but don't perform
//! actual tree-sitter parsing operations.
//!
//! # Graph Helpers
//!
//! For unified graph tests, use `sqry_test_support::graph_helpers` to inspect
//! staged call edges produced by `StagingGraph` operations.

pub mod graph_helpers;

use tree_sitter::Language;

/// Minimal test-only tree-sitter Language grammar
///
/// This struct mimics the layout of tree-sitter's `TSLanguage` (from parser.h).
/// It provides the minimum fields required for a valid Language handle that
/// passes tree-sitter's internal validation checks.
///
/// # Safety
///
/// This struct is carefully designed to match the exact memory layout of `TSLanguage`.
/// The field order and types are validated against tree-sitter 0.25.10 source code
/// (src/parser.h:107-136). Changes to tree-sitter's internal structure may require
/// updates to this definition.
///
/// # Layout (from tree-sitter/src/parser.h)
///
/// ```c
/// struct TSLanguage {
///   uint32_t abi_version;          // offset 0
///   uint32_t symbol_count;         // offset 4
///   uint32_t alias_count;          // offset 8
///   uint32_t token_count;          // offset 12
///   uint32_t external_token_count; // offset 16
///   uint32_t state_count;          // offset 20
///   uint32_t large_state_count;    // offset 24
///   uint32_t production_id_count;  // offset 28
///   uint32_t field_count;          // offset 32
///   uint16_t max_alias_sequence_length; // offset 36
///   const uint16_t *parse_table;   // offset 38/40 (depends on alignment)
///   // ... many more pointer fields
/// };
/// ```
///
/// We provide minimal valid values for the first fields and use null pointers
/// for the remaining fields (which are only accessed during actual parsing).
#[repr(C)]
struct TestTSLanguage {
    // Core metadata (always accessed)
    abi_version: u32,
    symbol_count: u32,
    alias_count: u32,
    token_count: u32,
    external_token_count: u32,
    state_count: u32,
    large_state_count: u32,
    production_id_count: u32,
    field_count: u32,
    max_alias_sequence_length: u16,

    // Padding for alignment (pointer follows)
    _padding: u16,

    // Parse table pointers (accessed during parsing - we set to null)
    // These are only safe to be null because we document that this grammar
    // should NOT be used for actual parsing
    parse_table: *const u16,
    small_parse_table: *const u16,
    small_parse_table_map: *const u32,

    // Additional pointers (all null, only accessed during parsing)
    parse_actions: *const u8, // TSParseActionEntry
    symbol_names: *const *const i8,
    field_names: *const *const i8,
    // Remaining fields omitted (not accessed by validation code)
}

// SAFETY: TestTSLanguage contains only integers and null pointers, no actual data
unsafe impl Sync for TestTSLanguage {}

/// Static test grammar instance
///
/// This grammar has minimal valid metadata:
/// - ABI version: Current tree-sitter `LANGUAGE_VERSION` (15 in v0.25)
/// - Symbol count: 1 (minimum for valid grammar)
/// - All other counts: 0 or 1 (minimal valid values)
/// - All pointers: null (safe because grammar is never used for parsing)
#[allow(
    clippy::cast_possible_truncation,
    reason = "tree-sitter language version fits in u32 for test grammar"
)]
const LANGUAGE_VERSION_U32: u32 = tree_sitter::LANGUAGE_VERSION as u32;

static TEST_GRAMMAR: TestTSLanguage = TestTSLanguage {
    abi_version: LANGUAGE_VERSION_U32,
    symbol_count: 1, // Minimum: 1 symbol (node_kind_count() = symbol_count + alias_count)
    alias_count: 0,
    token_count: 1,
    external_token_count: 0,
    state_count: 1, // Minimum: at least 1 state
    large_state_count: 0,
    production_id_count: 0,
    field_count: 0,
    max_alias_sequence_length: 0,
    _padding: 0,

    // All parse-related pointers are null (safe because we never parse with this grammar)
    parse_table: std::ptr::null(),
    small_parse_table: std::ptr::null(),
    small_parse_table_map: std::ptr::null(),
    parse_actions: std::ptr::null(),
    symbol_names: std::ptr::null(),
    field_names: std::ptr::null(),
};

/// Returns a minimal test-only tree-sitter Language
///
/// This function provides a safe alternative to `Language::from_raw(std::ptr::null())`
/// for testing purposes. The returned Language handle:
///
/// - Is **non-null** and passes tree-sitter's validation checks
/// - Has a **valid ABI version** (current tree-sitter `LANGUAGE_VERSION`)
/// - Has **minimal metadata** (1 symbol, 1 state)
/// - Lives for **'static lifetime** (safe to use anywhere)
/// - Should **NOT be used for actual parsing** (parse tables are null)
///
/// # Safety Guarantees
///
/// The returned Language is safe to use in any context that requires a valid
/// Language handle but does not perform actual parsing. Specifically:
///
/// - Calling `abi_version()` returns a valid version
/// - Calling `node_kind_count()` returns 1
/// - Passing to `sqry_tree_sitter_support::validate_language()` succeeds
/// - Using in plugin initialization that checks ABI compatibility works
///
/// # Warning
///
/// Do **NOT** use this Language for actual tree-sitter parsing operations.
/// The parse tables are all null pointers, so attempting to parse will cause
/// undefined behavior (likely a segfault).
///
/// # Example
///
/// ```rust
/// use sqry_test_support::test_language;
///
/// // Safe replacement for null Language
/// let lang = test_language();
///
/// // Can be used for metadata queries
/// assert_eq!(lang.node_kind_count(), 1);
/// assert_eq!(lang.abi_version(), tree_sitter::LANGUAGE_VERSION);
///
/// // Can be used in validation
/// use sqry_tree_sitter_support::validate_language;
/// assert!(validate_language(lang).is_ok());
/// ```
///
/// # Replaces Unsafe Pattern
///
/// Before:
/// ```rust,ignore
/// let lang = unsafe { Language::from_raw(std::ptr::null()) };  // UB!
/// ```
///
/// After:
/// ```rust
/// use sqry_test_support::test_language;
/// let lang = test_language();  // Safe!
/// ```
#[must_use = "Language handles should be used, not discarded"]
pub fn test_language() -> Language {
    // SAFETY: We create a Language from a pointer to our static TEST_GRAMMAR instance.
    // This is safe because:
    // 1. TEST_GRAMMAR is a valid TSLanguage struct with correct memory layout
    // 2. TEST_GRAMMAR has 'static lifetime (never deallocated)
    // 3. TEST_GRAMMAR has valid metadata that passes tree-sitter validation
    // 4. We document that this Language should not be used for actual parsing
    unsafe { Language::from_raw(std::ptr::from_ref(&TEST_GRAMMAR).cast()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_is_non_null() {
        let lang = test_language();
        let raw_ptr = lang.into_raw();
        assert!(
            !raw_ptr.is_null(),
            "Test language pointer should never be null"
        );
    }

    #[test]
    fn test_language_has_valid_abi() {
        let lang = test_language();
        let abi = lang.abi_version();
        assert_eq!(
            abi,
            tree_sitter::LANGUAGE_VERSION,
            "Test language should have current ABI version"
        );

        // Verify it's in the supported range
        let range = tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION;
        assert!(
            range.contains(&abi),
            "Test language ABI {} should be in supported range {:?}",
            abi,
            range
        );
    }

    #[test]
    fn test_language_has_nonzero_symbols() {
        let lang = test_language();
        let count = lang.node_kind_count();
        assert!(count > 0, "Test language must have at least one node kind");
        assert_eq!(
            count, 1,
            "Test language should have exactly 1 node kind (minimal)"
        );
    }

    #[test]
    fn test_language_passes_validation() {
        let lang = test_language();

        // Should pass sqry-tree-sitter-support validation
        use sqry_tree_sitter_support::validate_language;
        let result = validate_language(lang);
        assert!(
            result.is_ok(),
            "Test language should pass validation, got error: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_language_is_reusable() {
        // Test that we can call test_language() multiple times safely
        let lang1 = test_language();
        let lang2 = test_language();

        // Both should point to the same static instance
        assert_eq!(
            lang1.into_raw(),
            lang2.into_raw(),
            "Multiple calls to test_language() should return same instance"
        );
    }

    #[test]
    fn test_language_metadata() {
        let lang = test_language();

        // Verify basic metadata
        assert_eq!(lang.abi_version(), tree_sitter::LANGUAGE_VERSION);
        assert_eq!(lang.node_kind_count(), 1); // symbol_count + alias_count = 1 + 0
        assert_eq!(lang.parse_state_count(), 1); // Minimal state count
        assert_eq!(lang.field_count(), 0); // No fields needed
    }
}
