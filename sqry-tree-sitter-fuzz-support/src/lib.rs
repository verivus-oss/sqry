//! Malformed input generators for tree-sitter FFI safety testing.
//!
//! This crate provides comprehensive malformed input generation for testing
//! tree-sitter parsers' error handling and FFI boundary safety.
//!
//! # Features
//! - **Language Profiles**: 34 language-specific malformed patterns
//! - **UTF-8 Malformations**: Truncated sequences, invalid continuations, overlong encodings
//! - **Deep Nesting**: Language-aware deeply nested constructs
//! - **Oversized Inputs**: 1MB/10MB/100MB performance testing
//! - **Random Bytes**: Seeded PRNG for reproducible fuzzing
//! - **Stack-Safe Testing**: Dedicated threads for deep recursion tests
//!
//! # Example Usage
//! ```
//! use sqry_tree_sitter_fuzz_support::MalformedInputBuilder;
//! use sqry_tree_sitter_fuzz_support::generators::nesting::depths;
//!
//! // Generate deeply nested Rust code
//! let nested = MalformedInputBuilder::for_language("rust")
//!     .deeply_nested(depths::MEDIUM);
//!
//! // Generate truncated UTF-8
//! let truncated = MalformedInputBuilder::truncated_utf8();
//!
//! // Generate 1MB oversized input for Python
//! let large = MalformedInputBuilder::for_language("python")
//!     .oversized_1mb();
//! ```

pub mod generators;
pub mod profiles;
pub mod testing;

use profiles::get_profile;

/// Builder for generating malformed inputs.
///
/// Provides a fluent API for creating various types of malformed inputs
/// tailored to specific languages.
pub struct MalformedInputBuilder {
    language: &'static str,
}

impl MalformedInputBuilder {
    /// Creates a builder for a specific language.
    ///
    /// # Parameters
    /// - `language`: Language name (e.g., "rust", "python", "javascript")
    ///
    /// # Panics
    /// Panics if the language is not supported. Use `try_for_language` for
    /// non-panicking variant.
    ///
    /// # Supported Languages (34 total)
    /// All languages supported by sqry: rust, python, javascript, typescript,
    /// java, go, c, cpp, csharp, php, ruby, swift, kotlin, scala, r, perl,
    /// lua, shell, sql, plsql, html, css, dart, haskell, elixir, zig,
    /// terraform, puppet, apex, xanadu, groovy, abap, vue, svelte
    #[must_use]
    pub fn for_language(language: &'static str) -> Self {
        assert!(
            get_profile(language).is_some(),
            "Unsupported language: {language}. Use profiles::all_languages() to see supported languages."
        );
        Self { language }
    }

    /// Creates a builder for a specific language, returning None if unsupported.
    ///
    /// Non-panicking variant of `for_language`.
    #[must_use]
    pub fn try_for_language(language: &'static str) -> Option<Self> {
        get_profile(language).map(|_| Self { language })
    }

    /// Generates deeply nested constructs.
    ///
    /// # Parameters
    /// - `depth`: Number of nesting levels
    ///
    /// # Returns
    /// `Vec<u8>` with deeply nested structures.
    ///
    /// # Panics
    /// Panics if the language profile fails to generate nested input.
    #[must_use]
    pub fn deeply_nested(self, depth: usize) -> Vec<u8> {
        generators::generate_deeply_nested(self.language, depth)
            .expect("Language should be validated in constructor")
    }

    /// Generates a 1MB oversized input.
    ///
    /// Uses language-appropriate syntax repeated to fill 1MB.
    /// Safe for CI (always-on tests).
    #[must_use]
    pub fn oversized_1mb(self) -> Vec<u8> {
        generators::generate_1mb(self.language)
    }

    /// Generates a 10MB oversized input.
    ///
    /// Should be used with `#[ignore]` attribute in tests (nightly builds only).
    #[must_use]
    pub fn oversized_10mb(self) -> Vec<u8> {
        generators::generate_10mb(self.language)
    }

    /// Generates a 100MB oversized input.
    ///
    /// Should be used with `#[ignore]` attribute in tests (stress testing only).
    #[must_use]
    pub fn oversized_100mb(self) -> Vec<u8> {
        generators::generate_100mb(self.language)
    }

    // UTF-8 generators (language-independent)

    /// Generates input with truncated UTF-8 multi-byte sequences.
    #[must_use]
    pub fn truncated_utf8() -> Vec<u8> {
        generators::generate_truncated_utf8()
    }

    /// Generates input with invalid UTF-8 continuation bytes.
    #[must_use]
    pub fn invalid_continuation() -> Vec<u8> {
        generators::generate_invalid_continuation()
    }

    /// Generates input with overlong UTF-8 encodings.
    #[must_use]
    pub fn overlong_encoding() -> Vec<u8> {
        generators::generate_overlong_encoding()
    }

    /// Generates input with UTF-8 surrogate pair code points.
    #[must_use]
    pub fn surrogate_pairs() -> Vec<u8> {
        generators::generate_surrogate_pairs()
    }

    /// Generates input with embedded null bytes.
    #[must_use]
    pub fn null_bytes() -> Vec<u8> {
        generators::generate_null_bytes()
    }

    /// Generates random bytes using the default seed.
    ///
    /// # Parameters
    /// - `size`: Number of random bytes to generate
    #[must_use]
    pub fn random_bytes(size: usize) -> Vec<u8> {
        generators::generate_random_bytes_default(size)
    }

    /// Generates random bytes using a custom seed.
    ///
    /// # Parameters
    /// - `size`: Number of random bytes
    /// - `seed`: Seed for reproducibility
    #[must_use]
    pub fn random_bytes_seeded(size: usize, seed: u64) -> Vec<u8> {
        generators::generate_random_bytes(size, seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use generators::nesting::depths;
    use profiles::all_languages;

    #[test]
    fn test_builder_for_language() {
        let builder = MalformedInputBuilder::for_language("rust");
        assert_eq!(builder.language, "rust");
    }

    #[test]
    #[should_panic(expected = "Unsupported language")]
    fn test_builder_for_unsupported_language() {
        let _ = MalformedInputBuilder::for_language("unknown");
    }

    #[test]
    fn test_try_for_language() {
        assert!(MalformedInputBuilder::try_for_language("rust").is_some());
        assert!(MalformedInputBuilder::try_for_language("unknown").is_none());
    }

    #[test]
    fn test_deeply_nested() {
        let nested = MalformedInputBuilder::for_language("rust").deeply_nested(10);
        assert!(!nested.is_empty());

        let nested_str = String::from_utf8(nested).unwrap();
        assert_eq!(nested_str.matches('{').count(), 10);
    }

    #[test]
    fn test_oversized_1mb() {
        let large = MalformedInputBuilder::for_language("python").oversized_1mb();
        assert_eq!(large.len(), generators::sizes::MB_1);
    }

    #[test]
    #[ignore = "Performance test - run in nightly job to keep CI fast"]
    fn test_oversized_10mb() {
        let large = MalformedInputBuilder::for_language("java").oversized_10mb();
        assert_eq!(large.len(), generators::sizes::MB_10);
    }

    #[test]
    fn test_truncated_utf8() {
        let malformed = MalformedInputBuilder::truncated_utf8();
        assert!(!malformed.is_empty());
        assert!(std::str::from_utf8(&malformed).is_err());
    }

    #[test]
    fn test_invalid_continuation() {
        let malformed = MalformedInputBuilder::invalid_continuation();
        assert!(std::str::from_utf8(&malformed).is_err());
    }

    #[test]
    fn test_overlong_encoding() {
        let malformed = MalformedInputBuilder::overlong_encoding();
        assert!(std::str::from_utf8(&malformed).is_err());
    }

    #[test]
    fn test_surrogate_pairs() {
        let malformed = MalformedInputBuilder::surrogate_pairs();
        assert!(std::str::from_utf8(&malformed).is_err());
    }

    #[test]
    fn test_null_bytes() {
        let malformed = MalformedInputBuilder::null_bytes();
        assert!(malformed.contains(&0x00));
    }

    #[test]
    fn test_random_bytes() {
        let random = MalformedInputBuilder::random_bytes(100);
        assert_eq!(random.len(), 100);
    }

    #[test]
    fn test_random_bytes_seeded() {
        let r1 = MalformedInputBuilder::random_bytes_seeded(100, 12345);
        let r2 = MalformedInputBuilder::random_bytes_seeded(100, 12345);
        assert_eq!(r1, r2); // Same seed = same output
    }

    #[test]
    fn test_all_languages_work_with_builder() {
        for language in all_languages() {
            let builder = MalformedInputBuilder::for_language(language);
            let nested = builder.deeply_nested(depths::SHALLOW);
            assert!(
                !nested.is_empty(),
                "Language '{language}' should generate non-empty nested input"
            );
        }
    }
}
