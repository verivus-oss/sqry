//! Oversized input generators.
//!
//! Generates large inputs to test tree-sitter's handling of memory allocation
//! and performance with large files.

/// Size constants for oversized input testing.
pub mod sizes {
    /// 1 MB (always-on in CI).
    pub const MB_1: usize = 1024 * 1024;

    /// 10 MB (nightly builds only, marked with `#[ignore]`).
    pub const MB_10: usize = 10 * 1024 * 1024;

    /// 100 MB (stress testing only, marked with `#[ignore]`).
    pub const MB_100: usize = 100 * 1024 * 1024;
}

/// Generates oversized input by repeating a pattern.
///
/// # Parameters
/// - `pattern`: Byte pattern to repeat
/// - `target_size`: Desired total size in bytes
///
/// # Returns
/// `Vec<u8>` of approximately `target_size` bytes.
///
/// # Examples
/// ```
/// use sqry_tree_sitter_fuzz_support::generators::size::{generate_oversized, sizes};
///
/// let large = generate_oversized(b"fn main() {}\n", sizes::MB_1);
/// assert!(large.len() >= sizes::MB_1);
/// ```
#[must_use]
pub fn generate_oversized(pattern: &[u8], target_size: usize) -> Vec<u8> {
    if pattern.is_empty() {
        return vec![0; target_size];
    }

    let mut result = Vec::with_capacity(target_size);
    let pattern_len = pattern.len();
    let repetitions = target_size.div_ceil(pattern_len);

    for _ in 0..repetitions {
        result.extend_from_slice(pattern);
        if result.len() >= target_size {
            break;
        }
    }

    // Truncate to exact size if we overshot
    result.truncate(target_size);
    result
}

/// Generates a 1MB oversized input for a language.
///
/// Uses language-appropriate syntax that repeats to fill 1MB.
#[must_use]
pub fn generate_1mb(language: &str) -> Vec<u8> {
    let pattern = get_pattern_for_language(language);
    generate_oversized(pattern.as_bytes(), sizes::MB_1)
}

/// Generates a 10MB oversized input for a language.
///
/// Uses language-appropriate syntax that repeats to fill 10MB.
/// Should be marked with `#[ignore]` in tests.
#[must_use]
pub fn generate_10mb(language: &str) -> Vec<u8> {
    let pattern = get_pattern_for_language(language);
    generate_oversized(pattern.as_bytes(), sizes::MB_10)
}

/// Generates a 100MB oversized input for a language.
///
/// Uses language-appropriate syntax that repeats to fill 100MB.
/// Should be marked with `#[ignore]` in tests.
#[must_use]
pub fn generate_100mb(language: &str) -> Vec<u8> {
    let pattern = get_pattern_for_language(language);
    generate_oversized(pattern.as_bytes(), sizes::MB_100)
}

/// Returns a representative pattern for a language.
fn get_pattern_for_language(language: &str) -> &'static str {
    match language {
        "rust" => "fn f() { let x = 1; }\n",
        "python" => "def f():\n    pass\n",
        "javascript" | "typescript" => "function f() { return 1; }\n",
        "java" | "csharp" | "kotlin" => "class C { void m() {} }\n",
        "go" | "swift" => "func f() { return 1 }\n",
        "c" | "cpp" => "int f() { return 1; }\n",
        "sql" | "plsql" => "SELECT 1 FROM DUAL;\n",
        "shell" => "echo 'ok'\n",
        "html" | "vue" | "svelte" => "<div>content</div>\n",
        "css" => "body { margin: 0; }\n",
        "php" => "<?php echo 'ok'; ?>\n",
        "ruby" => "puts 'ok'\n",
        "lua" => "print('ok')\n",
        "groovy" => "println 'ok'\n",
        "scala" => "def f = 1\n",
        "dart" => "void f() {}\n",
        "haskell" => "f = 1\n",
        "zig" => "fn f() void {}\n",
        "apex" => "public class C {}\n",
        "terraform" => "resource \"null\" \"n\" {}\n",
        "puppet" => "class c {}\n",
        "xanadu" => "class C {}\n",
        "r" => "f <- function() {}\n",
        "elixir" => "defmodule M do end\n",
        "perl" => "sub f {}\n",
        "abap" => "WRITE 'ok'.\n",
        _ => "// generic pattern\n",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_oversized() {
        let pattern = b"test\n";
        let result = generate_oversized(pattern, 100);

        assert_eq!(result.len(), 100);
        assert!(result.windows(5).any(|w| w == pattern));
    }

    #[test]
    fn test_generate_oversized_exact_size() {
        let pattern = b"x";
        let result = generate_oversized(pattern, 1000);
        assert_eq!(result.len(), 1000);
    }

    #[test]
    fn test_generate_oversized_empty_pattern() {
        let result = generate_oversized(b"", 100);
        assert_eq!(result.len(), 100);
        assert!(result.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_1mb_generation() {
        let result = generate_1mb("rust");
        assert_eq!(result.len(), sizes::MB_1);
        assert!(result.windows(2).any(|w| w == b"fn"));
    }

    #[test]
    #[ignore = "Performance test - run in nightly job to keep CI fast"]
    fn test_10mb_generation() {
        let result = generate_10mb("python");
        assert_eq!(result.len(), sizes::MB_10);
    }

    #[test]
    #[ignore = "Performance test - run in nightly job to keep CI fast"]
    fn test_100mb_generation() {
        let result = generate_100mb("java");
        assert_eq!(result.len(), sizes::MB_100);
    }

    #[test]
    fn test_size_constants() {
        assert_eq!(sizes::MB_1, 1024 * 1024);
        assert_eq!(sizes::MB_10, 10 * 1024 * 1024);
        assert_eq!(sizes::MB_100, 100 * 1024 * 1024);
    }

    #[test]
    fn test_all_languages_have_patterns() {
        use crate::profiles::all_languages;

        for language in all_languages() {
            let pattern = get_pattern_for_language(language);
            assert!(
                !pattern.is_empty(),
                "Language '{language}' should have a pattern"
            );

            // Generate small sample to verify pattern works
            let sample = generate_oversized(pattern.as_bytes(), 1024);
            assert_eq!(sample.len(), 1024);
        }
    }
}
