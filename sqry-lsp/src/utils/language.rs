//! Language inference utilities
//!
//! Provides centralized language detection from file paths/extensions.

use std::path::Path;

/// Infer language from a file path based on extension.
/// Returns None for extensionless module paths (e.g., "requests", "./foo").
///
/// This is the authoritative language inference function for the LSP.
/// Supports 50+ language/extension mappings.
#[must_use]
pub fn infer_language_from_path(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    infer_language_from_extension(ext)
}

/// Infer language from a file extension string.
///
/// # Arguments
/// * `ext` - The file extension without the leading dot (e.g., "rs", "py")
///
/// # Returns
/// The language identifier if recognized, None otherwise.
#[must_use]
pub fn infer_language_from_extension(ext: &str) -> Option<String> {
    let lang = match ext.to_lowercase().as_str() {
        // Core languages (Tier 0)
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" | "pyw" | "pyi" => "python",
        "go" => "go",
        "java" => "java",

        // Systems languages (Tier 1)
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => "cpp",
        "cs" | "csx" => "csharp",
        "swift" => "swift",
        "dart" => "dart",
        "kt" | "kts" => "kotlin",
        "scala" | "sc" => "scala",
        "zig" => "zig",

        // Scripting languages (Tier 1/2)
        "rb" | "rake" | "gemspec" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "pl" | "pm" => "perl",
        "r" => "r",
        "sh" | "bash" | "zsh" => "shell",
        "groovy" | "gvy" | "gy" | "gsh" => "groovy",

        // Functional languages
        "hs" | "lhs" => "haskell",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "ml" | "mli" => "ocaml",
        "fs" | "fsx" | "fsi" => "fsharp",
        "clj" | "cljs" | "cljc" | "edn" => "clojure",
        "jl" => "julia",
        "nim" => "nim",
        "cr" => "crystal",

        // Frontend / markup
        "vue" => "vue",
        "svelte" => "svelte",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" | "less" => "css",

        // Data / query languages
        "sql" => "sql",
        "graphql" | "gql" => "graphql",

        // Infrastructure / config
        "tf" | "tfvars" => "terraform",
        "pp" => "puppet",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        "xml" => "xml",

        // Domain-specific
        "snjs" => "servicenow",
        "cls" | "trigger" => "apex",
        "abap" => "abap",
        "pks" | "pkb" | "pls" => "plsql",

        // Hardware description
        "v" | "vh" => "verilog",
        "vhd" | "vhdl" => "vhdl",

        // Protocol buffers
        "proto" => "protobuf",

        // Other
        "m" => "matlab",

        _ => return None,
    };
    Some(lang.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_core_languages() {
        assert_eq!(
            infer_language_from_path(Path::new("main.rs")),
            Some("rust".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("app.ts")),
            Some("typescript".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("script.py")),
            Some("python".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("main.go")),
            Some("go".to_string())
        );
    }

    #[test]
    fn test_tier1_languages() {
        assert_eq!(
            infer_language_from_path(Path::new("file.kt")),
            Some("kotlin".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("file.rb")),
            Some("ruby".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("file.scala")),
            Some("scala".to_string())
        );
    }

    #[test]
    fn test_tier2_languages() {
        assert_eq!(
            infer_language_from_path(Path::new("file.lua")),
            Some("lua".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("file.ex")),
            Some("elixir".to_string())
        );
        assert_eq!(
            infer_language_from_path(Path::new("file.hs")),
            Some("haskell".to_string())
        );
    }

    #[test]
    fn test_extensionless_returns_none() {
        assert_eq!(infer_language_from_path(Path::new("requests")), None);
        assert_eq!(infer_language_from_path(Path::new("./foo")), None);
    }

    #[test]
    fn test_unknown_extension_returns_none() {
        assert_eq!(infer_language_from_path(Path::new("file.xyz")), None);
    }
}
