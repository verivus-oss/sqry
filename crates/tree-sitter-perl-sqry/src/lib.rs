//! Tree-sitter grammar for Perl (vendored for sqry)
//!
//! The Rust binding in this crate is first-party (maintained in the sqry
//! repository). The grammar under grammar-src/ is vendored THIRD-PARTY code
//! reproduced under its upstream license; the full license text and copyright
//! notice ship next to the sources in grammar-src/LICENSE and are also recorded
//! in the repository-root THIRD-PARTY-LICENSES file.
//!
//! **Source Grammar**: <https://github.com/tree-sitter-perl/tree-sitter-perl>
//! **Release**: v1.2.1 (published crate `ts-parser-perl` 1.2.1)
//! **Commit**: c3e17b31179bf8f658c9f37c7a3ea6a202212d5a
//! **License**: MIT

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_perl() -> Language;
}

/// Returns the tree-sitter Language for Perl
#[must_use = "Language handles must be registered with tree-sitter consumers"]
pub fn language() -> Language {
    let lang = unsafe { tree_sitter_perl() };
    sqry_tree_sitter_support::validate_language_or_panic(lang, "Perl")
}

/// Fallible alternative to [`language()`]
#[allow(clippy::missing_errors_doc)] // Vendored tree-sitter binding
pub fn try_language() -> Result<Language, sqry_tree_sitter_support::TreeSitterError> {
    let lang = unsafe { tree_sitter_perl() };
    sqry_tree_sitter_support::validate_language(lang)
}

/// The content of the node-types.json file for this grammar.
pub const NODE_TYPES: &str = include_str!("../grammar-src/node-types.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_can_load_grammar() {
        let lang = language();
        assert!(lang.abi_version() > 0);
    }

    #[test]
    fn test_try_language_succeeds() {
        assert!(try_language().is_ok());
    }

    #[test]
    #[allow(clippy::const_is_empty)]
    fn test_node_types_not_empty() {
        assert!(!NODE_TYPES.is_empty());
    }
}
