//! File type classification for document handling.
//!
//! This module provides file type detection based on extensions to determine:
//! - Whether a file is supported (text-based) or unsupported (binary)
//! - Which size limit category applies to the file
//!
//! # Categories
//!
//! - **Source Code**: Programming languages and markup (`.rs`, `.js`, `.py`, etc.)
//! - **Data**: Structured data files that can grow large (`.json`, `.xml`, `.yaml`, etc.)
//! - **Binary**: Non-text files that should be rejected entirely (`.pdf`, `.png`, `.zip`, etc.)
//! - **Unknown**: Files without recognized extensions, treated as source code

use std::path::Path;

/// File type category for document handling decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    /// Source code files - programming languages and markup.
    /// Default limit: 512 KB
    SourceCode,

    /// Structured data files that can grow large.
    /// Default limit: 10 MB (JSON knowledge graphs, large configs, etc.)
    Data,

    /// Binary files - not processable as text.
    /// These should be rejected entirely, not by size.
    Binary,

    /// Unknown extension - treat as source code.
    Unknown,
}

impl FileCategory {
    /// Returns true if this file category is supported for LSP processing.
    #[must_use]
    pub fn is_supported(self) -> bool {
        !matches!(self, Self::Binary)
    }

    /// Returns a human-readable description of the category.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::SourceCode => "source code",
            Self::Data => "data file",
            Self::Binary => "binary file",
            Self::Unknown => "unknown file type",
        }
    }
}

const SOURCE_EXTENSIONS: &[&str] = &[
    "rs",
    "rust",
    "js",
    "mjs",
    "cjs",
    "ts",
    "mts",
    "cts",
    "tsx",
    "jsx",
    "py",
    "pyi",
    "pyw",
    "go",
    "java",
    "kt",
    "kts",
    "scala",
    "groovy",
    "gradle",
    "c",
    "h",
    "cpp",
    "hpp",
    "cc",
    "cxx",
    "hxx",
    "c++",
    "h++",
    "cs",
    "fs",
    "fsx",
    "vb",
    "rb",
    "rake",
    "gemspec",
    "php",
    "phtml",
    "swift",
    "m",
    "mm",
    "lua",
    "pl",
    "pm",
    "pod",
    "r",
    "rmd",
    "jl",
    "ex",
    "exs",
    "erl",
    "hrl",
    "hs",
    "lhs",
    "clj",
    "cljs",
    "cljc",
    "edn",
    "ml",
    "mli",
    "mll",
    "mly",
    "nim",
    "nims",
    "zig",
    "v",
    "sv",
    "svh",
    "vh",
    "vhd",
    "vhdl",
    "tcl",
    "tk",
    "awk",
    "sed",
    "ps1",
    "psm1",
    "psd1",
    "bat",
    "cmd",
    // Shell scripts
    "sh",
    "bash",
    "zsh",
    "fish",
    "ksh",
    "csh",
    "tcsh",
    // Configuration as code / IaC
    "tf",
    "tfvars",
    "hcl",
    "pp",
    "nix",
    "dhall",
    // Query languages
    "sql",
    "psql",
    "mysql",
    "pgsql",
    "plsql",
    "graphql",
    "gql",
    "cypher",
    "cql",
    // Markup and templates
    "html",
    "htm",
    "xhtml",
    "css",
    "scss",
    "sass",
    "less",
    "styl",
    "vue",
    "svelte",
    "erb",
    "haml",
    "slim",
    "ejs",
    "hbs",
    "mustache",
    "njk",
    "pug",
    "jade",
    "jinja",
    "jinja2",
    "j2",
    "twig",
    "liquid",
    // Documentation markup
    "md",
    "markdown",
    "mdown",
    "mkdn",
    "mdx",
    "rst",
    "rest",
    "adoc",
    "asciidoc",
    "tex",
    "latex",
    "ltx",
    "sty",
    "cls",
    "org",
    "wiki",
    "mediawiki",
    // Build and project files
    "makefile",
    "mk",
    "cmake",
    "meson",
    "dockerfile",
    "vagrantfile",
    "justfile",
    "rakefile",
    "gemfile",
    "podfile",
    "fastfile",
    "jenkinsfile",
    "snakefile",
    "sconscript",
    "sconstruct",
];

const DATA_EXTENSIONS: &[&str] = &[
    "json",
    "jsonl",
    "ndjson",
    "json5",
    "jsonc",
    "xml",
    "xsl",
    "xslt",
    "xsd",
    "dtd",
    "svg",
    "yaml",
    "yml",
    "toml",
    "csv",
    "tsv",
    "psv",
    "ini",
    "cfg",
    "conf",
    "config",
    "properties",
    "env",
    "plist",
    "log",
    "lock",
];

const BINARY_EXTENSIONS: &[&str] = &[
    // Images
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "icns", "webp", "avif", "heic", "heif", "tiff",
    "tif", "psd", "ai", "eps", "raw", "cr2", "nef", "dng", // Audio/video
    "mp3", "mp4", "wav", "flac", "ogg", "opus", "m4a", "aac", "avi", "mov", "wmv", "mkv", "webm",
    "m4v", "mpg", "mpeg", "3gp", "flv", "swf", // Documents
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "odg", "rtf", "epub",
    "mobi", "azw", "azw3", // Archives
    "zip", "rar", "7z", "gz", "bz2", "xz", "lz", "lzma", "zst", "tar", "tgz", "tbz", "tbz2", "txz",
    "cab", "dmg", "iso", "img", "deb", "rpm", "apk", "snap", "flatpak", "appimage",
    // Executables and libraries
    "exe", "dll", "so", "dylib", "a", "lib", "o", "obj", "bin", "out", "elf", "class", "pyc", "pyo",
    "pyd", "wasm", "wat", "ko", "sys", "drv", // Fonts
    "ttf", "otf", "woff", "woff2", "eot", "fon", "fnt", // Databases
    "db", "sqlite", "sqlite3", "mdb", "accdb", "ldb", // Serialized data
    "pickle", "pkl", "npy", "npz", "h5", "hdf5", "parquet", "avro", "orc", "feather", "arrow",
    "protobuf", "pb", "msgpack", "bson", "rdb", // Certificates and keys
    "der", "cer", "crt", "p7b", "p7c", "p12", "pfx", // Other
    "swp", "swo", "swn", "lnk", "url",
];

/// Classifies a file based on its extension.
///
/// # Examples
///
/// ```
/// use sqry_lsp::file_types::{classify_file, FileCategory};
/// use std::path::Path;
///
/// assert_eq!(classify_file(Path::new("main.rs")), FileCategory::SourceCode);
/// assert_eq!(classify_file(Path::new("config.json")), FileCategory::Data);
/// assert_eq!(classify_file(Path::new("document.pdf")), FileCategory::Binary);
/// ```
#[must_use]
pub fn classify_file(path: &Path) -> FileCategory {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(e) => e.to_ascii_lowercase(),
        None => return FileCategory::Unknown,
    };

    let ext = ext.as_str();
    if SOURCE_EXTENSIONS.contains(&ext) {
        return FileCategory::SourceCode;
    }
    if DATA_EXTENSIONS.contains(&ext) {
        return FileCategory::Data;
    }
    if BINARY_EXTENSIONS.contains(&ext) {
        return FileCategory::Binary;
    }
    FileCategory::Unknown
}

/// Returns true if the file at the given path is a binary file that should be rejected.
#[must_use]
pub fn is_binary_file(path: &Path) -> bool {
    matches!(classify_file(path), FileCategory::Binary)
}

/// Returns true if the file at the given path is a data file (JSON, XML, etc.)
/// that may have larger size limits.
#[must_use]
pub fn is_data_file(path: &Path) -> bool {
    matches!(classify_file(path), FileCategory::Data)
}

/// Returns true if the file at the given path is source code.
#[must_use]
pub fn is_source_code(path: &Path) -> bool {
    matches!(
        classify_file(path),
        FileCategory::SourceCode | FileCategory::Unknown
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rust_as_source() {
        assert_eq!(
            classify_file(Path::new("main.rs")),
            FileCategory::SourceCode
        );
        assert_eq!(
            classify_file(Path::new("/foo/bar/lib.rs")),
            FileCategory::SourceCode
        );
    }

    #[test]
    fn classifies_common_languages() {
        let sources = [
            "app.js",
            "index.ts",
            "main.py",
            "server.go",
            "Main.java",
            "program.c",
            "lib.cpp",
            "script.rb",
            "app.php",
            "main.swift",
        ];
        for name in sources {
            assert_eq!(
                classify_file(Path::new(name)),
                FileCategory::SourceCode,
                "expected {name} to be source code"
            );
        }
    }

    #[test]
    fn classifies_json_as_data() {
        assert_eq!(classify_file(Path::new("package.json")), FileCategory::Data);
        assert_eq!(classify_file(Path::new("graph.json")), FileCategory::Data);
        assert_eq!(classify_file(Path::new("config.yaml")), FileCategory::Data);
        assert_eq!(classify_file(Path::new("data.xml")), FileCategory::Data);
        assert_eq!(classify_file(Path::new("Cargo.lock")), FileCategory::Data);
    }

    #[test]
    fn classifies_pdf_as_binary() {
        assert_eq!(
            classify_file(Path::new("document.pdf")),
            FileCategory::Binary
        );
        assert_eq!(
            classify_file(Path::new("/path/to/G175p.pdf")),
            FileCategory::Binary
        );
    }

    #[test]
    fn classifies_images_as_binary() {
        let binaries = [
            "photo.jpg",
            "icon.png",
            "animation.gif",
            "logo.svg", // Note: SVG is XML-based but classified as Data
        ];
        for name in binaries {
            let category = classify_file(Path::new(name));
            // SVG is an exception - it's text-based XML
            #[allow(clippy::case_sensitive_file_extension_comparisons)]
            // File type detection handles case in logic
            if name.ends_with(".svg") {
                assert_eq!(
                    category,
                    FileCategory::Data,
                    "expected {name} to be data (XML)"
                );
            } else {
                assert_eq!(
                    category,
                    FileCategory::Binary,
                    "expected {name} to be binary"
                );
            }
        }
    }

    #[test]
    fn classifies_archives_as_binary() {
        let archives = ["file.zip", "archive.tar.gz", "package.7z", "backup.rar"];
        for name in archives {
            // For compound extensions like .tar.gz, we get the last extension
            let category = classify_file(Path::new(name));
            assert_eq!(
                category,
                FileCategory::Binary,
                "expected {name} to be binary"
            );
        }
    }

    #[test]
    fn classifies_office_docs_as_binary() {
        let docs = [
            "report.docx",
            "spreadsheet.xlsx",
            "presentation.pptx",
            "legacy.doc",
        ];
        for name in docs {
            assert_eq!(
                classify_file(Path::new(name)),
                FileCategory::Binary,
                "expected {name} to be binary"
            );
        }
    }

    #[test]
    fn unknown_extension_is_unknown() {
        assert_eq!(
            classify_file(Path::new("file.xyz123")),
            FileCategory::Unknown
        );
        assert_eq!(
            classify_file(Path::new("noextension")),
            FileCategory::Unknown
        );
    }

    #[test]
    fn is_supported_rejects_binary() {
        assert!(FileCategory::SourceCode.is_supported());
        assert!(FileCategory::Data.is_supported());
        assert!(FileCategory::Unknown.is_supported());
        assert!(!FileCategory::Binary.is_supported());
    }

    #[test]
    fn helper_functions_work() {
        assert!(is_binary_file(Path::new("doc.pdf")));
        assert!(!is_binary_file(Path::new("main.rs")));

        assert!(is_data_file(Path::new("config.json")));
        assert!(!is_data_file(Path::new("main.rs")));

        assert!(is_source_code(Path::new("main.rs")));
        assert!(is_source_code(Path::new("unknown.xyz"))); // Unknown treated as source
        assert!(!is_source_code(Path::new("data.json")));
    }

    #[test]
    fn case_insensitive_extension() {
        assert_eq!(classify_file(Path::new("IMAGE.PNG")), FileCategory::Binary);
        assert_eq!(classify_file(Path::new("data.JSON")), FileCategory::Data);
        assert_eq!(
            classify_file(Path::new("Main.RS")),
            FileCategory::SourceCode
        );
    }

    #[test]
    fn terraform_and_iac_are_source() {
        assert_eq!(
            classify_file(Path::new("main.tf")),
            FileCategory::SourceCode
        );
        assert_eq!(
            classify_file(Path::new("vars.tfvars")),
            FileCategory::SourceCode
        );
        assert_eq!(
            classify_file(Path::new("manifest.pp")),
            FileCategory::SourceCode
        );
    }

    #[test]
    fn sql_files_are_source() {
        assert_eq!(
            classify_file(Path::new("schema.sql")),
            FileCategory::SourceCode
        );
        assert_eq!(
            classify_file(Path::new("queries.psql")),
            FileCategory::SourceCode
        );
    }
}
