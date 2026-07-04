//! Tree-sitter grammar for Elixir (vendored for sqry)
//!
//! The Rust binding in this crate is first-party (maintained in the sqry
//! repository). The grammar under grammar-src/ is vendored THIRD-PARTY code
//! reproduced under its upstream license; the full license text and copyright
//! notice are in the repository-root THIRD-PARTY-LICENSES file.
//!
//! **Source Grammar**: <https://github.com/elixir-lang/tree-sitter-elixir>
//! **Commit**: 5c22791c9836d436ce31de5e454fbad0e706ea96
//! **License**: Apache-2.0

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_elixir() -> Language;
}

/// Returns the tree-sitter Language for Elixir
#[must_use = "Language handles must be registered with tree-sitter consumers"]
pub fn language() -> Language {
    let lang = unsafe { tree_sitter_elixir() };
    sqry_tree_sitter_support::validate_language_or_panic(lang, "Elixir")
}

/// Fallible alternative to [`language()`]
#[allow(clippy::missing_errors_doc)] // Vendored tree-sitter binding
pub fn try_language() -> Result<Language, sqry_tree_sitter_support::TreeSitterError> {
    let lang = unsafe { tree_sitter_elixir() };
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
