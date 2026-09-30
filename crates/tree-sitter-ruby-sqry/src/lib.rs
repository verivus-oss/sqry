//! Tree-sitter grammar for Ruby (vendored for sqry)
//!
//! The Rust binding in this crate is first-party (maintained in the sqry
//! repository). The grammar under grammar-src/ is vendored THIRD-PARTY code
//! reproduced under its upstream license; the full license text and copyright
//! notice ship next to the sources in grammar-src/LICENSE and are also recorded
//! in the repository-root THIRD-PARTY-LICENSES file.
//!
//! **Source Grammar**: <https://github.com/tree-sitter/tree-sitter-ruby>
//! **Base release**: v0.23.1 (published crate `tree-sitter-ruby` 0.23.1)
//! **License**: MIT
//!
//! # Why this grammar is vendored
//!
//! The published 0.23.1 external scanner overflows its serialization buffer on
//! heredocs, which aborts the process through `assert(size == length)` in
//! `deserialize`. Two defects combine: the bounds guard in `serialize`
//! under-counts the bytes the loop body writes, and the heredoc identifier
//! length is stored in a single byte, so an identifier longer than 255
//! characters wraps. The abort is reachable in ordinary release builds, so any
//! indexed Ruby file of that shape terminates the CLI, the daemon, the LSP
//! server or the MCP server.
//!
//! Upstream fixed both in commit `ad907a69da0c` ("scanner: fix heredoc
//! serialization buffer overflows", 2026-03-10) by widening the length field to
//! `uint32_t` on the write and read sides and correcting the guard. That commit
//! has never been released: 0.23.1 (2024-11-11) is still the newest version on
//! crates.io, and the repository has taken four commits in the twenty-two months
//! since. A git dependency is not an option because this workspace publishes to
//! crates.io, so the grammar is vendored with the upstream fix applied verbatim.
//!
//! `grammar-src/scanner.c` is byte-identical to upstream master at
//! `ad907a69da0c` (SHA-256 `88c1c036d5af7c22a1bc9cc5e50411d414ac9624afed676d42957c566ac4083d`).
//! Every other file under grammar-src/ is unmodified from the published 0.23.1
//! crate.
//!
//! # Retirement condition
//!
//! Upstream issue [#269](https://github.com/tree-sitter/tree-sitter-ruby/issues/269)
//! tracks the crash. When a release containing `ad907a69da0c` reaches crates.io,
//! delete this crate and point `sqry-lang-ruby` back at the published
//! `tree-sitter-ruby`.

use tree_sitter::Language;

unsafe extern "C" {
    fn tree_sitter_ruby() -> Language;
}

/// Returns the tree-sitter Language for Ruby
#[must_use = "Language handles must be registered with tree-sitter consumers"]
pub fn language() -> Language {
    let lang = unsafe { tree_sitter_ruby() };
    sqry_tree_sitter_support::validate_language_or_panic(lang, "Ruby")
}

/// Fallible alternative to [`language()`]
#[allow(clippy::missing_errors_doc)] // Vendored tree-sitter binding
pub fn try_language() -> Result<Language, sqry_tree_sitter_support::TreeSitterError> {
    let lang = unsafe { tree_sitter_ruby() };
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

    /// Regression for the heredoc serialization overflow (sqry issue #747,
    /// upstream tree-sitter-ruby issue #269).
    ///
    /// The published 0.23.1 scanner stored the heredoc identifier length in a
    /// single byte, so an identifier longer than 255 characters wrapped and
    /// `deserialize` then read a different number of bytes than `serialize`
    /// wrote, tripping `assert(size == length)` and aborting the process.
    #[test]
    fn heredoc_identifier_longer_than_255_chars_does_not_abort() {
        let word = "A".repeat(300);
        let source = format!("<<~{word}\ncontent\n{word}\n");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language())
            .expect("Ruby grammar should load");
        let tree = parser.parse(&source, None).expect("parse should return");
        assert_eq!(tree.root_node().kind(), "program");
    }

    /// The other half of the same defect: the bounds guard in `serialize`
    /// admitted a heredoc it did not have room for. The guard counted
    /// `size + 2 + word` while the loop body wrote `4 + word` (three flags, a
    /// one-byte length, then the identifier), so a state that ended two bytes
    /// under the 1024-byte buffer was written one byte past it.
    ///
    /// The window is narrow. With no string literals on the stack the
    /// serialized state starts at two bytes and each heredoc adds `4 + word`,
    /// so only a seven-character identifier lands inside it, on the
    /// ninety-third heredoc: the guard sees 1023 and the write ends at 1025.
    /// The sweep covers the arithmetic being off by a heredoc either way.
    #[test]
    fn serialize_guard_admits_no_heredoc_it_cannot_fit() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language())
            .expect("Ruby grammar should load");

        for count in 88..=96 {
            // Seven-character identifiers, and no string literals, so the
            // serialized layout matches the accounting above.
            let openers: Vec<String> = (0..count).map(|i| format!("<<~H{i:06}")).collect();
            let source = format!("x = [{}]\n", openers.join(", "));
            let tree = parser
                .parse(&source, None)
                .expect("parse should return for every heredoc count");
            assert_eq!(tree.root_node().kind(), "program");
        }
    }

    /// Incremental reparse, isolated to the incremental path.
    ///
    /// An earlier version of this test opened a long-identifier heredoc in its
    /// very first parse, so under the previous revision it aborted before the
    /// reparse ran. It passed for a reason unrelated to the name on it. Here
    /// the first parse is deliberately benign, a short identifier the previous
    /// revision's one-byte length field represents exactly, and only the edit
    /// introduces an identifier past that limit. An abort in this test can
    /// therefore only come from the reparse. Verified against the previous
    /// revision: the first parse alone succeeds, and only the reparse aborts.
    #[test]
    fn incremental_reparse_round_trips_heredoc_scanner_state() {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language())
            .expect("Ruby grammar should load");

        let first = "x = <<~SHORT\ncontent\nSHORT\n";
        let mut tree = parser.parse(first, None).expect("parse should return");
        assert_eq!(tree.root_node().kind(), "program");

        let word = "B".repeat(300);
        let appended = format!("y = <<~{word}\ncontent\n{word}\n");
        let second = format!("{first}{appended}");
        tree.edit(&tree_sitter::InputEdit {
            start_byte: first.len(),
            old_end_byte: first.len(),
            new_end_byte: second.len(),
            start_position: tree_sitter::Point::new(3, 0),
            old_end_position: tree_sitter::Point::new(3, 0),
            new_end_position: tree_sitter::Point::new(6, 0),
        });
        let reparsed = parser
            .parse(&second, Some(&tree))
            .expect("incremental parse should return");
        assert_eq!(reparsed.root_node().kind(), "program");
    }
}
