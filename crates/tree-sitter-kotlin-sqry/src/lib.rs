//! Tree-sitter grammar for Kotlin (vendored for sqry)
//!
//! This is a first-party binding maintained in the sqry repository.
//!
//! **Source Grammar**: <https://github.com/fwcd/tree-sitter-kotlin>
//! **Commit**: f3a1ea74304adad67164a0a6ffe729428748a7a7
//! **License**: MIT

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_kotlin() -> Language;
}

/// Returns the tree-sitter Language for Kotlin
#[must_use = "Language handles must be registered with tree-sitter consumers"]
pub fn language() -> Language {
    let lang = unsafe { tree_sitter_kotlin() };
    sqry_tree_sitter_support::validate_language_or_panic(lang, "Kotlin")
}

/// Fallible alternative to [`language()`]
pub fn try_language() -> Result<Language, sqry_tree_sitter_support::TreeSitterError> {
    let lang = unsafe { tree_sitter_kotlin() };
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
