//! Safe tree-sitter FFI validation support for sqry
//!
//! This crate provides validation helpers for tree-sitter Language handles loaded via FFI.
//! All tree-sitter language bindings in the sqry ecosystem should use these helpers to ensure
//! memory safety and ABI compatibility.
//!
//! # Safety Invariants
//!
//! Tree-sitter languages are loaded via `unsafe` FFI calls to C libraries. This crate ensures:
//! 1. **Null-pointer guard**: Language pointers are validated as non-null before any dereferencing
//! 2. **ABI version compatibility**: Dynamically enforced based on tree-sitter runtime constants
//! 3. **Language version requirements**: Ensures compatibility with tree-sitter minimum version
//! 4. **Grammar validity**: Non-zero node count sanity check
//! 5. **Structured error reporting**: No silent UB, all failures produce typed errors
//!
//! # Usage
//!
//! ```rust,no_run
//! use tree_sitter::Language;
//! use sqry_tree_sitter_support::{validate_language, TreeSitterError};
//!
//! unsafe extern "C" {
//!     fn tree_sitter_rust() -> Language;
//! }
//!
//! pub fn language() -> Language {
//!     // SAFETY: tree_sitter_rust() is an extern C function from the linked
//!     // tree-sitter-rust library. We immediately validate the returned Language
//!     // handle against ABI/version requirements. If validation fails, we panic
//!     // (per infallible API contract). The C library is assumed to be correctly
//!     // built and linked (cargo build-time invariant).
//!     let lang = unsafe { tree_sitter_rust() };
//!     validate_language(lang)
//!         .unwrap_or_else(|e| panic!("Rust grammar validation failed: {}", e))
//! }
//! ```

use std::ops::RangeInclusive;
use thiserror::Error;
use tree_sitter::Language;

/// Errors that can occur during tree-sitter Language validation
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TreeSitterError {
    /// The ABI version of the loaded grammar is outside the supported range
    #[error("ABI version mismatch: found {found}, expected range {expected_range:?}")]
    AbiVersionMismatch {
        /// The ABI version found in the loaded grammar
        found: usize,
        /// The range of supported ABI versions
        expected_range: RangeInclusive<usize>,
    },

    /// The language version is below the minimum compatible version
    #[error("Incompatible language version: found {found}, minimum required {min_required}")]
    IncompatibleVersion {
        /// The language version found
        found: usize,
        /// The minimum required version
        min_required: usize,
    },

    /// The loaded grammar appears invalid (e.g., zero nodes)
    #[error("Invalid grammar: {reason}")]
    InvalidGrammar {
        /// Description of why the grammar is invalid
        reason: &'static str,
    },

    /// A null Language pointer was encountered (should never happen with modern tree-sitter)
    #[error("Null language pointer encountered")]
    NullLanguagePointer,
}

/// Returns the supported ABI version range for tree-sitter grammars
///
/// The range is dynamically derived from tree-sitter's runtime constants to ensure
/// compatibility as the library evolves. Currently this returns the range from
/// `MIN_COMPATIBLE_LANGUAGE_VERSION` to `LANGUAGE_VERSION`.
///
/// Grammars compiled with ABI versions outside this range will be rejected.
#[must_use]
pub const fn supported_abi_range() -> RangeInclusive<usize> {
    tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION..=tree_sitter::LANGUAGE_VERSION
}

/// Validates a tree-sitter Language handle for safety and compatibility
///
/// # Safety Checks
///
/// This function performs the following validation in order:
/// 0. **Null-pointer guard** (CRITICAL): Verifies the Language pointer is non-null before any method calls
/// 1. **ABI version check**: Ensures the grammar ABI version is within the supported range (dynamically derived from tree-sitter constants)
/// 2. **`MIN_COMPATIBLE_LANGUAGE_VERSION` check**: Ensures the language version meets tree-sitter's minimum compatibility requirements
/// 3. **Grammar validity check**: Ensures the grammar has at least one node kind (sanity check)
///
/// # UB Prevention
///
/// The null-pointer check (Check 0) happens BEFORE any method calls on the Language handle,
/// preventing undefined behavior from dereferencing null/corrupt pointers. Subsequent checks
/// are safe because the `Language` type is an opaque handle, and calling `abi_version()` and
/// `node_kind_count()` is safe even if the grammar is malformed (tree-sitter validates these
/// internally).
///
/// # Arguments
///
/// * `lang` - The Language handle returned from an unsafe `tree_sitter_*()` FFI call
///
/// # Returns
///
/// * `Ok(lang)` - The validated Language handle, safe to use
/// * `Err(TreeSitterError)` - Detailed error about why validation failed
///
/// # Errors
///
/// Returns `Err(TreeSitterError)` if the Language pointer is null, the ABI or
/// language version is incompatible, or the grammar reports zero node kinds.
///
/// # Example
///
/// ```rust,no_run
/// # use tree_sitter::Language;
/// # use sqry_tree_sitter_support::validate_language;
/// # unsafe extern "C" { fn tree_sitter_rust() -> Language; }
/// let lang = unsafe { tree_sitter_rust() };
/// match validate_language(lang) {
///     Ok(validated_lang) => {
///         // Safe to use
///         println!("Grammar loaded: {} node types", validated_lang.node_kind_count());
///     }
///     Err(e) => {
///         eprintln!("Failed to load grammar: {}", e);
///     }
/// }
/// ```
pub fn validate_language(lang: Language) -> Result<Language, TreeSitterError> {
    // Check 0: Null pointer guard (CRITICAL)
    // SAFETY: We must verify the Language handle is non-null BEFORE calling any
    // methods on it (abi_version(), node_kind_count(), etc.) which would dereference
    // the internal pointer. This guard prevents UB if the extern C function returns
    // a null or corrupted pointer.
    //
    // Since Language doesn't expose as_ptr(), we use into_raw() to extract the pointer.
    // This consumes the Language via ManuallyDrop (no drop/deallocation occurs).
    let raw_ptr = lang.into_raw();

    // Check if the pointer is null BEFORE reconstructing the Language
    if raw_ptr.is_null() {
        return Err(TreeSitterError::NullLanguagePointer);
    }

    // SAFETY: We just verified the pointer is non-null, which satisfies
    // the safety requirement of from_raw(). No deallocation occurred in
    // into_raw() (it uses ManuallyDrop), so the pointer is still valid.
    let lang = unsafe { Language::from_raw(raw_ptr) };

    // From this point forward, the pointer is known non-null and we can safely
    // call methods that dereference it.

    // Check 1: ABI version range
    let abi = lang.abi_version();
    let supported_range = supported_abi_range();
    if !supported_range.contains(&abi) {
        return Err(TreeSitterError::AbiVersionMismatch {
            found: abi,
            expected_range: supported_range,
        });
    }

    // Check 2: Minimum compatible version
    // tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION is typically 13, but we use
    // the constant to future-proof against tree-sitter library updates
    if abi < tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION {
        return Err(TreeSitterError::IncompatibleVersion {
            found: abi,
            min_required: tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION,
        });
    }

    // Check 3: Grammar validity (node count > 0)
    let node_count = lang.node_kind_count();
    if node_count == 0 {
        return Err(TreeSitterError::InvalidGrammar {
            reason: "node_kind_count is zero",
        });
    }

    // All checks passed
    Ok(lang)
}

/// Validates a tree-sitter Language and returns it or panics with a detailed message
///
/// This is a convenience wrapper around [`validate_language`] for use in infallible
/// `pub fn language() -> Language` APIs. If validation fails, this function panics
/// with a descriptive error message.
///
/// # Panics
///
/// Panics if the Language fails any validation check. The panic message includes
/// the specific validation error and the grammar name.
///
/// # Arguments
///
/// * `lang` - The Language handle to validate
/// * `grammar_name` - Human-readable name of the grammar (e.g., "Rust", "Python")
///
/// # Example
///
/// ```rust,no_run
/// # use tree_sitter::Language;
/// # use sqry_tree_sitter_support::validate_language_or_panic;
/// # unsafe extern "C" { fn tree_sitter_rust() -> Language; }
/// pub fn language() -> Language {
///     let lang = unsafe { tree_sitter_rust() };
///     validate_language_or_panic(lang, "Rust")
/// }
/// ```
#[must_use]
pub fn validate_language_or_panic(lang: Language, grammar_name: &str) -> Language {
    validate_language(lang)
        .unwrap_or_else(|e| panic!("{grammar_name} grammar validation failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    #[test]
    fn test_abi_range_constants() {
        let range = supported_abi_range();
        // Verify the range is derived from tree-sitter constants
        assert_eq!(*range.start(), tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION);
        assert_eq!(*range.end(), tree_sitter::LANGUAGE_VERSION);
        // As of tree-sitter 0.25, this should be 13..=15
        assert!(*range.start() >= 13, "MIN_COMPATIBLE should be at least 13");
        assert!(*range.end() >= 15, "LANGUAGE_VERSION should be at least 15");
    }

    #[test]
    fn test_reject_null_pointer() {
        // SAFETY: We deliberately construct an invalid (null) Language pointer
        // to test that validate_language correctly rejects it with NullLanguagePointer error.
        // This is a test-only operation and the null Language is never dereferenced.
        let null_lang = unsafe { Language::from_raw(ptr::null()) };

        let result = validate_language(null_lang);
        assert!(
            result.is_err(),
            "validate_language should reject null pointer"
        );

        match result.unwrap_err() {
            TreeSitterError::NullLanguagePointer => {
                // Expected error variant
            }
            other => panic!("Expected NullLanguagePointer, got {other:?}"),
        }
    }

    #[test]
    #[should_panic(expected = "grammar validation failed")]
    fn test_validate_language_or_panic_on_null() {
        // SAFETY: Test-only null Language construction to verify panic behavior
        let null_lang = unsafe { Language::from_raw(ptr::null()) };

        // This should panic with the expected message
        let _ = validate_language_or_panic(null_lang, "TestGrammar");
    }

    #[test]
    fn test_error_display() {
        let err = TreeSitterError::AbiVersionMismatch {
            found: 8,
            expected_range: supported_abi_range(),
        };
        assert!(err.to_string().contains("ABI version mismatch"));
        assert!(err.to_string().contains('8'));

        let err = TreeSitterError::IncompatibleVersion {
            found: 10,
            min_required: 13,
        };
        assert!(err.to_string().contains("Incompatible language version"));
        assert!(err.to_string().contains("10"));
        assert!(err.to_string().contains("13"));

        let err = TreeSitterError::InvalidGrammar {
            reason: "test reason",
        };
        assert!(err.to_string().contains("Invalid grammar"));
        assert!(err.to_string().contains("test reason"));

        let err = TreeSitterError::NullLanguagePointer;
        assert!(err.to_string().contains("Null language pointer"));
    }

    #[test]
    fn test_error_equality() {
        let err1 = TreeSitterError::AbiVersionMismatch {
            found: 8,
            expected_range: 9..=14,
        };
        let err2 = TreeSitterError::AbiVersionMismatch {
            found: 8,
            expected_range: 9..=14,
        };
        assert_eq!(err1, err2);

        let err3 = TreeSitterError::AbiVersionMismatch {
            found: 7,
            expected_range: 9..=14,
        };
        assert_ne!(err1, err3);
    }

    // Test-only FFI infrastructure for creating controlled Language instances
    //
    // SAFETY: These helpers create minimal TSLanguage stubs for testing validation paths.
    // They match the memory layout of tree-sitter's TSLanguage struct (from parser.h):
    //   struct TSLanguage {
    //     uint32_t abi_version;   // offset 0
    //     uint32_t symbol_count;  // offset 4
    //     uint32_t alias_count;   // offset 8
    //     uint32_t token_count;   // offset 12
    //     ... // many more fields we don't need for validation tests
    //   }
    //
    // NOTE: node_kind_count() calls ts_language_symbol_count() which returns
    // symbol_count + alias_count (validated in tree-sitter/src/language.c:19-21).
    //
    // These stubs ONLY set the first three fields and should NEVER be used for actual parsing.
    // They exist solely to test the validation logic in validate_language().

    #[repr(C)]
    struct TestTSLanguage {
        abi_version: u32,
        symbol_count: u32,
        alias_count: u32,
        // We omit the remaining ~30+ fields since validation only checks these three
    }

    // Static test instances with controlled ABI/symbol_count values
    static TEST_LANG_LOW_ABI: TestTSLanguage = TestTSLanguage {
        abi_version: 8, // Below MIN_COMPATIBLE_LANGUAGE_VERSION (13)
        symbol_count: 100,
        alias_count: 0,
    };

    static TEST_LANG_HIGH_ABI: TestTSLanguage = TestTSLanguage {
        abi_version: 20, // Above LANGUAGE_VERSION (15)
        symbol_count: 100,
        alias_count: 0,
    };

    #[allow(
        clippy::cast_possible_truncation,
        reason = "tree-sitter language versions fit in u32 for test stubs"
    )]
    const LANGUAGE_VERSION_U32: u32 = tree_sitter::LANGUAGE_VERSION as u32;

    static TEST_LANG_ZERO_NODES: TestTSLanguage = TestTSLanguage {
        abi_version: LANGUAGE_VERSION_U32,
        symbol_count: 0, // Invalid: grammars must have at least one node
        alias_count: 0,  // node_kind_count() = symbol_count + alias_count = 0
    };

    // Test-only helper to create a Language from a test stub
    //
    // SAFETY: This function is test-only and creates a Language handle pointing to
    // one of the static test instances above. The returned Language should NEVER be
    // used for actual parsing - it exists solely to test validation error paths.
    unsafe fn test_language_from_stub(stub: &'static TestTSLanguage) -> Language {
        // SAFETY: Caller ensures this is test-only usage and the stub lives for 'static.
        // We cast the TestTSLanguage pointer to TSLanguage pointer for tree-sitter FFI.
        unsafe { Language::from_raw(std::ptr::from_ref(stub).cast()) }
    }

    #[test]
    fn test_reject_low_abi_version() {
        // SAFETY: Test-only Language with ABI version below minimum (8 < 13)
        let lang = unsafe { test_language_from_stub(&TEST_LANG_LOW_ABI) };

        let result = validate_language(lang);
        assert!(result.is_err(), "Should reject low ABI version");

        match result.unwrap_err() {
            TreeSitterError::AbiVersionMismatch { found, .. } => {
                assert_eq!(found, 8, "Should report ABI version 8");
                assert!(found < tree_sitter::MIN_COMPATIBLE_LANGUAGE_VERSION);
            }
            other => panic!("Expected AbiVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_reject_high_abi_version() {
        // SAFETY: Test-only Language with ABI version above maximum (20 > 15)
        let lang = unsafe { test_language_from_stub(&TEST_LANG_HIGH_ABI) };

        let result = validate_language(lang);
        assert!(result.is_err(), "Should reject high ABI version");

        match result.unwrap_err() {
            TreeSitterError::AbiVersionMismatch { found, .. } => {
                assert_eq!(found, 20, "Should report ABI version 20");
                assert!(found > tree_sitter::LANGUAGE_VERSION);
            }
            other => panic!("Expected AbiVersionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn test_reject_zero_node_count() {
        // SAFETY: Test-only Language with zero node kinds (invalid grammar)
        let lang = unsafe { test_language_from_stub(&TEST_LANG_ZERO_NODES) };

        let result = validate_language(lang);
        assert!(result.is_err(), "Should reject zero node count");

        match result.unwrap_err() {
            TreeSitterError::InvalidGrammar { reason } => {
                assert!(
                    reason.contains("node"),
                    "Error should mention node count: {reason}"
                );
            }
            other => panic!("Expected InvalidGrammar, got {other:?}"),
        }
    }

    // NOTE: test_reject_incompatible_version is omitted because we cannot easily
    // fabricate a Language with ABI version in range (13..=15) but with an incompatible
    // language version (the version field is not exposed in TSLanguage struct layout,
    // it's computed from the ABI version by tree-sitter internally).
    // The MIN_COMPATIBLE_VERSION check is still executed in production code, but creating
    // a test case would require either:
    //  1. Using a real grammar compiled with an old tree-sitter version (fragile)
    //  2. Mocking the entire TSLanguage struct with all ~30+ fields (extremely fragile)
    //
    // Since the ABI version range check (13..=15) already covers the practical cases,
    // and the incompatible version check is defensive (tree-sitter guarantees version
    // is tied to ABI), we accept this gap in negative-path test coverage as a pragmatic
    // trade-off against test maintenance burden.
}
