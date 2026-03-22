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

    // ── infer_language_from_extension — exhaustive coverage ─────────────────

    #[test]
    fn test_tsx_jsx_extensions() {
        assert_eq!(
            infer_language_from_extension("tsx"),
            Some("typescript".to_string())
        );
        assert_eq!(
            infer_language_from_extension("jsx"),
            Some("javascript".to_string())
        );
        assert_eq!(
            infer_language_from_extension("mjs"),
            Some("javascript".to_string())
        );
        assert_eq!(
            infer_language_from_extension("cjs"),
            Some("javascript".to_string())
        );
    }

    #[test]
    fn test_python_variants() {
        assert_eq!(
            infer_language_from_extension("pyw"),
            Some("python".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pyi"),
            Some("python".to_string())
        );
    }

    #[test]
    fn test_c_cpp_variants() {
        assert_eq!(infer_language_from_extension("c"), Some("c".to_string()));
        assert_eq!(infer_language_from_extension("h"), Some("c".to_string()));
        assert_eq!(
            infer_language_from_extension("cpp"),
            Some("cpp".to_string())
        );
        assert_eq!(infer_language_from_extension("cc"), Some("cpp".to_string()));
        assert_eq!(
            infer_language_from_extension("cxx"),
            Some("cpp".to_string())
        );
        assert_eq!(
            infer_language_from_extension("hpp"),
            Some("cpp".to_string())
        );
        assert_eq!(
            infer_language_from_extension("hxx"),
            Some("cpp".to_string())
        );
    }

    #[test]
    fn test_csharp() {
        assert_eq!(
            infer_language_from_extension("cs"),
            Some("csharp".to_string())
        );
        assert_eq!(
            infer_language_from_extension("csx"),
            Some("csharp".to_string())
        );
    }

    #[test]
    fn test_swift_dart_zig() {
        assert_eq!(
            infer_language_from_extension("swift"),
            Some("swift".to_string())
        );
        assert_eq!(
            infer_language_from_extension("dart"),
            Some("dart".to_string())
        );
        assert_eq!(
            infer_language_from_extension("zig"),
            Some("zig".to_string())
        );
    }

    #[test]
    fn test_kotlin_scala() {
        assert_eq!(
            infer_language_from_extension("kts"),
            Some("kotlin".to_string())
        );
        assert_eq!(
            infer_language_from_extension("sc"),
            Some("scala".to_string())
        );
    }

    #[test]
    fn test_scripting_languages() {
        assert_eq!(
            infer_language_from_extension("rake"),
            Some("ruby".to_string())
        );
        assert_eq!(
            infer_language_from_extension("gemspec"),
            Some("ruby".to_string())
        );
        assert_eq!(
            infer_language_from_extension("php"),
            Some("php".to_string())
        );
        assert_eq!(
            infer_language_from_extension("lua"),
            Some("lua".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pl"),
            Some("perl".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pm"),
            Some("perl".to_string())
        );
        assert_eq!(infer_language_from_extension("r"), Some("r".to_string()));
        assert_eq!(
            infer_language_from_extension("bash"),
            Some("shell".to_string())
        );
        assert_eq!(
            infer_language_from_extension("zsh"),
            Some("shell".to_string())
        );
    }

    #[test]
    fn test_groovy_variants() {
        assert_eq!(
            infer_language_from_extension("groovy"),
            Some("groovy".to_string())
        );
        assert_eq!(
            infer_language_from_extension("gvy"),
            Some("groovy".to_string())
        );
        assert_eq!(
            infer_language_from_extension("gy"),
            Some("groovy".to_string())
        );
        assert_eq!(
            infer_language_from_extension("gsh"),
            Some("groovy".to_string())
        );
    }

    #[test]
    fn test_functional_languages() {
        assert_eq!(
            infer_language_from_extension("lhs"),
            Some("haskell".to_string())
        );
        assert_eq!(
            infer_language_from_extension("exs"),
            Some("elixir".to_string())
        );
        assert_eq!(
            infer_language_from_extension("erl"),
            Some("erlang".to_string())
        );
        assert_eq!(
            infer_language_from_extension("hrl"),
            Some("erlang".to_string())
        );
        assert_eq!(
            infer_language_from_extension("ml"),
            Some("ocaml".to_string())
        );
        assert_eq!(
            infer_language_from_extension("mli"),
            Some("ocaml".to_string())
        );
        assert_eq!(
            infer_language_from_extension("fs"),
            Some("fsharp".to_string())
        );
        assert_eq!(
            infer_language_from_extension("fsx"),
            Some("fsharp".to_string())
        );
        assert_eq!(
            infer_language_from_extension("fsi"),
            Some("fsharp".to_string())
        );
        assert_eq!(
            infer_language_from_extension("clj"),
            Some("clojure".to_string())
        );
        assert_eq!(
            infer_language_from_extension("cljs"),
            Some("clojure".to_string())
        );
        assert_eq!(
            infer_language_from_extension("cljc"),
            Some("clojure".to_string())
        );
        assert_eq!(
            infer_language_from_extension("edn"),
            Some("clojure".to_string())
        );
        assert_eq!(
            infer_language_from_extension("jl"),
            Some("julia".to_string())
        );
        assert_eq!(
            infer_language_from_extension("nim"),
            Some("nim".to_string())
        );
        assert_eq!(
            infer_language_from_extension("cr"),
            Some("crystal".to_string())
        );
    }

    #[test]
    fn test_frontend_markup() {
        assert_eq!(
            infer_language_from_extension("vue"),
            Some("vue".to_string())
        );
        assert_eq!(
            infer_language_from_extension("svelte"),
            Some("svelte".to_string())
        );
        assert_eq!(
            infer_language_from_extension("html"),
            Some("html".to_string())
        );
        assert_eq!(
            infer_language_from_extension("htm"),
            Some("html".to_string())
        );
        assert_eq!(
            infer_language_from_extension("css"),
            Some("css".to_string())
        );
        assert_eq!(
            infer_language_from_extension("scss"),
            Some("css".to_string())
        );
        assert_eq!(
            infer_language_from_extension("sass"),
            Some("css".to_string())
        );
        assert_eq!(
            infer_language_from_extension("less"),
            Some("css".to_string())
        );
    }

    #[test]
    fn test_data_query_languages() {
        assert_eq!(
            infer_language_from_extension("sql"),
            Some("sql".to_string())
        );
        assert_eq!(
            infer_language_from_extension("graphql"),
            Some("graphql".to_string())
        );
        assert_eq!(
            infer_language_from_extension("gql"),
            Some("graphql".to_string())
        );
    }

    #[test]
    fn test_infra_config_languages() {
        assert_eq!(
            infer_language_from_extension("tf"),
            Some("terraform".to_string())
        );
        assert_eq!(
            infer_language_from_extension("tfvars"),
            Some("terraform".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pp"),
            Some("puppet".to_string())
        );
        assert_eq!(
            infer_language_from_extension("yaml"),
            Some("yaml".to_string())
        );
        assert_eq!(
            infer_language_from_extension("yml"),
            Some("yaml".to_string())
        );
        assert_eq!(
            infer_language_from_extension("json"),
            Some("json".to_string())
        );
        assert_eq!(
            infer_language_from_extension("toml"),
            Some("toml".to_string())
        );
        assert_eq!(
            infer_language_from_extension("xml"),
            Some("xml".to_string())
        );
    }

    #[test]
    fn test_domain_specific_languages() {
        assert_eq!(
            infer_language_from_extension("snjs"),
            Some("servicenow".to_string())
        );
        assert_eq!(
            infer_language_from_extension("cls"),
            Some("apex".to_string())
        );
        assert_eq!(
            infer_language_from_extension("trigger"),
            Some("apex".to_string())
        );
        assert_eq!(
            infer_language_from_extension("abap"),
            Some("abap".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pks"),
            Some("plsql".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pkb"),
            Some("plsql".to_string())
        );
        assert_eq!(
            infer_language_from_extension("pls"),
            Some("plsql".to_string())
        );
    }

    #[test]
    fn test_hardware_description() {
        assert_eq!(
            infer_language_from_extension("v"),
            Some("verilog".to_string())
        );
        assert_eq!(
            infer_language_from_extension("vh"),
            Some("verilog".to_string())
        );
        assert_eq!(
            infer_language_from_extension("vhd"),
            Some("vhdl".to_string())
        );
        assert_eq!(
            infer_language_from_extension("vhdl"),
            Some("vhdl".to_string())
        );
    }

    #[test]
    fn test_protobuf_matlab() {
        assert_eq!(
            infer_language_from_extension("proto"),
            Some("protobuf".to_string())
        );
        assert_eq!(
            infer_language_from_extension("m"),
            Some("matlab".to_string())
        );
    }

    #[test]
    fn test_case_insensitive_extension() {
        assert_eq!(
            infer_language_from_extension("RS"),
            Some("rust".to_string())
        );
        assert_eq!(
            infer_language_from_extension("PY"),
            Some("python".to_string())
        );
    }
}
