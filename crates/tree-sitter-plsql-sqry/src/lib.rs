//! Tree-sitter grammar for Oracle PL/SQL (vendored for sqry)
//!
//! This crate wraps the tree-sitter-plsql grammar from:
//! <https://github.com/AndreasMaierDe/tree-sitter-plsql>
//!
//! Grammar commit: 28aebef (2022-12-11)
//! License: MIT

use tree_sitter::Language;

extern "C" {
    fn tree_sitter_plsql() -> Language;
}

/// Returns the tree-sitter Language for PL/SQL
///
/// # Panics
///
/// Panics if the loaded grammar fails validation:
/// - Null pointer returned from C library
/// - ABI version mismatch (outside 9-14 range)
/// - Incompatible version (below tree-sitter minimum)
/// - Invalid grammar (zero node kinds)
///
/// These failures indicate a serious build or linking problem that cannot
/// be recovered from at runtime.
///
/// # Safety
///
/// This function wraps an `unsafe` FFI call to `tree_sitter_plsql()`.
/// The returned Language handle is validated before being exposed to prevent
/// undefined behavior from invalid pointers or ABI mismatches.
#[must_use = "Language handles must be registered with tree-sitter consumers"]
pub fn language() -> Language {
    // SAFETY: tree_sitter_plsql() is an extern C function from the linked
    // tree-sitter-plsql library. We immediately validate the returned Language
    // handle (including null-pointer check) before any dereference occurs.
    // If validation fails, we panic (per infallible API contract).
    // The C library is assumed to be correctly built and linked (cargo
    // build-time invariant).
    let lang = unsafe { tree_sitter_plsql() };
    sqry_tree_sitter_support::validate_language_or_panic(lang, "PL/SQL")
}

/// Returns the tree-sitter Language for PL/SQL, with error handling
///
/// This is a fallible alternative to [`language()`] for use cases that need
/// to handle grammar loading errors gracefully.
///
/// # Errors
///
/// Returns an error if the loaded grammar fails validation. See
/// [`sqry_tree_sitter_support::TreeSitterError`] for possible error variants.
pub fn try_language() -> Result<Language, sqry_tree_sitter_support::TreeSitterError> {
    // SAFETY: Same as language(), but with Result instead of panic
    let lang = unsafe { tree_sitter_plsql() };
    sqry_tree_sitter_support::validate_language(lang)
}

/// The node types JSON for PL/SQL grammar
pub const NODE_TYPES: &str = include_str!("../../../vendor/tree-sitter-plsql/src/node-types.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_load_grammar() {
        let lang = language();
        assert!(
            lang.abi_version() > 0,
            "Language ABI version should be non-zero"
        );
    }

    #[test]
    fn test_try_language_succeeds() {
        let result = try_language();
        assert!(
            result.is_ok(),
            "try_language() should succeed for valid grammar"
        );
        let lang = result.unwrap();
        assert!(lang.abi_version() > 0);
    }
}
