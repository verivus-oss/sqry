//! Tree-sitter grammar for Vue (vendored for sqry)
//!
//! This is a first-party binding maintained in the sqry repository.
//!
//! **Source Grammar**: <https://github.com/ikatyang/tree-sitter-vue>
//! **Commit**: 91fe2754796cd8fba5f229505a23fa08f3546c06
//! **Date**: 2021-04-04
//! **License**: MIT

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_vue() -> Language;
}

/// Returns the tree-sitter Language for Vue
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
/// This function wraps an `unsafe` FFI call to `tree_sitter_vue()`.
/// The returned Language handle is validated before being exposed to prevent
/// undefined behavior from invalid pointers or ABI mismatches.
#[must_use = "Language handles must be registered with tree-sitter consumers"]
pub fn language() -> Language {
    // SAFETY: tree_sitter_vue() is an extern C function from the linked
    // tree-sitter-vue library. We immediately validate the returned Language
    // handle (including null-pointer check) before any dereference occurs.
    // If validation fails, we panic (per infallible API contract).
    // The C library is assumed to be correctly built and linked (cargo
    // build-time invariant).
    let lang = unsafe { tree_sitter_vue() };
    sqry_tree_sitter_support::validate_language_or_panic(lang, "Vue")
}

/// Returns the tree-sitter Language for Vue, with error handling
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
    let lang = unsafe { tree_sitter_vue() };
    sqry_tree_sitter_support::validate_language(lang)
}

/// The content of the [`node-types.json`][] file for this grammar.
///
/// [`node-types.json`]: https://tree-sitter.github.io/tree-sitter/using-parsers#static-node-types
pub const NODE_TYPES: &str = include_str!("../../../vendor/tree-sitter-vue/src/node-types.json");

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

    #[test]
    #[allow(clippy::const_is_empty)]
    fn test_node_types_not_empty() {
        assert!(!NODE_TYPES.is_empty(), "Node types should not be empty");
    }
}
