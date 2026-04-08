//! Tree-sitter grammar for Perl (vendored for sqry)
//!
//! This is a first-party binding maintained in the sqry repository.
//!
//! **Source Grammar**: <https://github.com/tree-sitter-perl/tree-sitter-perl>
//! **Commit**: 0c24d001dd1921e418fb933d208a7bd7dd3f923a
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
