//! Deep nesting generator using language profiles.
//!
//! Generates deeply nested constructs that stress-test tree-sitter's
//! parsing stack and error recovery mechanisms.

use crate::profiles::get_profile;

/// Generates deeply nested constructs for a specific language.
///
/// # Parameters
/// - `language`: Language name (e.g., "rust", "python")
/// - `depth`: Number of nesting levels (e.g., 500, 1000)
///
/// # Returns
/// `Some(Vec<u8>)` if the language is supported, `None` otherwise.
///
/// # Examples
/// ```
/// use sqry_tree_sitter_fuzz_support::generators::nesting::generate_deeply_nested;
///
/// let nested = generate_deeply_nested("rust", 500).expect("Rust should be supported");
/// assert!(!nested.is_empty());
/// ```
#[must_use]
pub fn generate_deeply_nested(language: &str, depth: usize) -> Option<Vec<u8>> {
    get_profile(language).map(|profile| profile.generate_deeply_nested(depth))
}

/// Common nesting depths for testing.
pub mod depths {
    /// Shallow nesting (quick test).
    pub const SHALLOW: usize = 100;

    /// Medium nesting (standard test).
    pub const MEDIUM: usize = 500;

    /// Deep nesting (stress test).
    pub const DEEP: usize = 1000;

    /// Extreme nesting (may cause stack overflow without mitigation).
    pub const EXTREME: usize = 5000;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::all_languages;

    #[test]
    fn test_all_languages_support_nesting() {
        for language in all_languages() {
            let nested = generate_deeply_nested(language, 10)
                .unwrap_or_else(|| panic!("Language '{language}' should support nesting"));
            assert!(
                !nested.is_empty(),
                "Language '{language}' should generate non-empty nested input"
            );
        }
    }

    #[test]
    fn test_unknown_language() {
        let result = generate_deeply_nested("unknown", 10);
        assert!(result.is_none());
    }

    #[test]
    fn test_depth_constants() {
        assert_eq!(depths::SHALLOW, 100);
        assert_eq!(depths::MEDIUM, 500);
        assert_eq!(depths::DEEP, 1000);
        assert_eq!(depths::EXTREME, 5000);
    }

    #[test]
    fn test_rust_deep_nesting() {
        let nested = generate_deeply_nested("rust", depths::MEDIUM).unwrap();
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 500 opening and closing braces
        assert_eq!(nested_str.matches('{').count(), 500);
        assert_eq!(nested_str.matches('}').count(), 500);
    }

    #[test]
    fn test_python_deep_nesting() {
        let nested = generate_deeply_nested("python", depths::SHALLOW).unwrap();
        let nested_str = String::from_utf8(nested).unwrap();

        // Should have 100 if statements
        assert_eq!(nested_str.matches("if True:").count(), 100);
        assert!(nested_str.contains("pass"));
    }

    #[test]
    fn test_different_depths() {
        let shallow = generate_deeply_nested("java", depths::SHALLOW).unwrap();
        let deep = generate_deeply_nested("java", depths::DEEP).unwrap();

        // Deep nesting should produce longer output
        assert!(deep.len() > shallow.len());
    }
}
