//! Language profiles for malformed input generation.
//!
//! Each profile defines how to generate language-specific malformed patterns
//! that stress-test tree-sitter parsers' error handling capabilities.

mod curly_brace;
mod hybrid;
mod indentation;
mod keyword;
mod template;

pub use curly_brace::*;
pub use hybrid::*;
pub use indentation::*;
pub use keyword::*;
pub use template::*;

/// Trait for generating language-specific malformed input patterns.
///
/// Each language profile implements this trait to provide:
/// - Deep nesting constructs (blocks, expressions, etc.)
/// - Language-specific syntax patterns
/// - Edge cases that stress tree-sitter's error recovery
pub trait LanguageProfile: Send + Sync {
    /// Returns the language name (e.g., "rust", "python").
    fn language_name(&self) -> &'static str;

    /// Generates deeply nested constructs.
    ///
    /// # Parameters
    /// - `depth`: Number of nesting levels (e.g., 500, 1000)
    ///
    /// # Returns
    /// Valid UTF-8 bytes with deeply nested structures
    fn generate_deeply_nested(&self, depth: usize) -> Vec<u8>;

    /// Returns a minimal valid syntax pattern for the language.
    ///
    /// Used as a baseline to verify the profile is working.
    fn minimal_valid(&self) -> &'static str;
}

/// Language family enumeration for profile classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguageFamily {
    /// Curly-brace languages (C-family): Rust, C, C++, Java, JavaScript, etc.
    CurlyBrace,
    /// Indentation-based languages: Python, ABAP
    Indentation,
    /// Keyword-based languages: SQL, Shell, Lua, Groovy
    Keyword,
    /// Template languages: HTML, Vue, Svelte
    Template,
    /// Hybrid languages with multiple syntactic styles: PHP, Ruby
    Hybrid,
}

/// Retrieves a language profile by name.
///
/// # Parameters
/// - `language`: Language name (e.g., "rust", "python", "javascript")
///
/// # Returns
/// `Some(Box<dyn LanguageProfile>)` if the language is supported, `None` otherwise.
///
/// # Supported Languages (34 total)
/// - **`CurlyBrace` (22)**: rust, c, cpp, java, javascript, typescript, go, csharp,
///   kotlin, swift, scala, dart, haskell, zig, apex, terraform, puppet, xanadu, css, r, elixir, perl
/// - **Indentation (2)**: python, abap
/// - **Keyword (4)**: sql, shell, lua, groovy
/// - **Template (3)**: html, vue, svelte
/// - **Hybrid (2)**: php, ruby
/// - **PL/SQL (1)**: plsql (special keyword-based variant)
#[must_use]
pub fn get_profile(language: &str) -> Option<Box<dyn LanguageProfile>> {
    match language {
        // CurlyBrace family (22 languages)
        "rust" => Some(Box::new(RustProfile)),
        "c" => Some(Box::new(CProfile)),
        "cpp" => Some(Box::new(CppProfile)),
        "java" => Some(Box::new(JavaProfile)),
        "javascript" => Some(Box::new(JavaScriptProfile)),
        "typescript" => Some(Box::new(TypeScriptProfile)),
        "go" => Some(Box::new(GoProfile)),
        "csharp" => Some(Box::new(CSharpProfile)),
        "kotlin" => Some(Box::new(KotlinProfile)),
        "swift" => Some(Box::new(SwiftProfile)),
        "scala" => Some(Box::new(ScalaProfile)),
        "dart" => Some(Box::new(DartProfile)),
        "haskell" => Some(Box::new(HaskellProfile)),
        "zig" => Some(Box::new(ZigProfile)),
        "apex" => Some(Box::new(ApexProfile)),
        "terraform" => Some(Box::new(TerraformProfile)),
        "puppet" => Some(Box::new(PuppetProfile)),
        "xanadu" => Some(Box::new(XanaduProfile)),
        "css" => Some(Box::new(CssProfile)),
        "r" => Some(Box::new(RProfile)),
        "elixir" => Some(Box::new(ElixirProfile)),
        "perl" => Some(Box::new(PerlProfile)),

        // Indentation family (2 languages)
        "python" => Some(Box::new(PythonProfile)),
        "abap" => Some(Box::new(AbapProfile)),

        // Keyword family (4 languages)
        "sql" => Some(Box::new(SqlProfile)),
        "shell" => Some(Box::new(ShellProfile)),
        "lua" => Some(Box::new(LuaProfile)),
        "groovy" => Some(Box::new(GroovyProfile)),

        // Template family (3 languages)
        "html" => Some(Box::new(HtmlProfile)),
        "vue" => Some(Box::new(VueProfile)),
        "svelte" => Some(Box::new(SvelteProfile)),

        // Hybrid family (2 languages)
        "php" => Some(Box::new(PhpProfile)),
        "ruby" => Some(Box::new(RubyProfile)),

        // PL/SQL (1 language - special keyword-based variant)
        "plsql" => Some(Box::new(PlsqlProfile)),

        _ => None,
    }
}

/// Returns the language family for a given language name.
#[must_use]
pub fn get_language_family(language: &str) -> Option<LanguageFamily> {
    match language {
        "rust" | "c" | "cpp" | "java" | "javascript" | "typescript" | "go" | "csharp"
        | "kotlin" | "swift" | "scala" | "dart" | "haskell" | "zig" | "apex" | "terraform"
        | "puppet" | "xanadu" | "css" | "r" | "elixir" | "perl" => Some(LanguageFamily::CurlyBrace),

        "python" | "abap" => Some(LanguageFamily::Indentation),

        "sql" | "shell" | "lua" | "groovy" | "plsql" => Some(LanguageFamily::Keyword),

        "html" | "vue" | "svelte" => Some(LanguageFamily::Template),

        "php" | "ruby" => Some(LanguageFamily::Hybrid),

        _ => None,
    }
}

/// Returns all supported language names.
#[must_use]
pub fn all_languages() -> Vec<&'static str> {
    vec![
        // CurlyBrace (22)
        "rust",
        "c",
        "cpp",
        "java",
        "javascript",
        "typescript",
        "go",
        "csharp",
        "kotlin",
        "swift",
        "scala",
        "dart",
        "haskell",
        "zig",
        "apex",
        "terraform",
        "puppet",
        "xanadu",
        "css",
        "r",
        "elixir",
        "perl",
        // Indentation (2)
        "python",
        "abap",
        // Keyword (4)
        "sql",
        "shell",
        "lua",
        "groovy",
        // Template (3)
        "html",
        "vue",
        "svelte",
        // Hybrid (2)
        "php",
        "ruby",
        // PL/SQL (1)
        "plsql",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_languages_have_profiles() {
        for language in all_languages() {
            assert!(
                get_profile(language).is_some(),
                "Language '{language}' should have a profile"
            );
        }
    }

    #[test]
    fn test_profile_count() {
        assert_eq!(
            all_languages().len(),
            34,
            "Should have 34 language profiles"
        );
    }

    #[test]
    fn test_language_families() {
        // CurlyBrace: 22 languages
        assert_eq!(
            get_language_family("rust"),
            Some(LanguageFamily::CurlyBrace)
        );
        assert_eq!(
            get_language_family("java"),
            Some(LanguageFamily::CurlyBrace)
        );

        // Indentation: 2 languages
        assert_eq!(
            get_language_family("python"),
            Some(LanguageFamily::Indentation)
        );
        assert_eq!(
            get_language_family("abap"),
            Some(LanguageFamily::Indentation)
        );

        // Keyword: 5 languages (including plsql)
        assert_eq!(get_language_family("sql"), Some(LanguageFamily::Keyword));
        assert_eq!(get_language_family("plsql"), Some(LanguageFamily::Keyword));

        // Template: 3 languages
        assert_eq!(get_language_family("html"), Some(LanguageFamily::Template));

        // Hybrid: 2 languages
        assert_eq!(get_language_family("php"), Some(LanguageFamily::Hybrid));
        assert_eq!(get_language_family("ruby"), Some(LanguageFamily::Hybrid));

        // Unknown
        assert_eq!(get_language_family("unknown"), None);
    }

    #[test]
    fn test_minimal_valid() {
        for language in all_languages() {
            let profile = get_profile(language).expect("Profile should exist");
            let minimal = profile.minimal_valid();
            assert!(
                !minimal.is_empty(),
                "Language '{language}' should have non-empty minimal valid syntax"
            );
        }
    }
}
