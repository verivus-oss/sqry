//! Project-level configuration for sqry
//!
//! Implements configuration loading per `02_DESIGN.md` \[M3\]:
//! - `.sqry-config.toml` search from `index_root` up to filesystem root
//! - Supports ignore patterns, language hints, indexing limits
//! - Error handling with graceful fallback to defaults
//!
//! # Configuration File Location
//!
//! The config file is searched starting from `index_root` and walking UP:
//! 1. Start at `index_root`
//! 2. Check for `.sqry-config.toml` in current directory
//! 3. If found: load and stop; else move to parent
//! 4. Continue until filesystem root or found
//! 5. If not found: use default configuration
//!
//! # What Configuration Does NOT Control
//!
//! Per `PROJECT_ROOT_SPEC.md` Section 4.2:
//! - Index root selection (always CLI path or mode-derived)
//! - `ProjectRootMode` (LSP setting only)
//! - Project boundaries (determined by mode)

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Configuration file name
pub const CONFIG_FILE_NAME: &str = ".sqry-config.toml";

/// Errors that can occur when loading configuration
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Config file not found in ancestry
    #[error("Configuration file not found")]
    NotFound,

    /// Config file found but failed to parse
    #[error("Failed to parse config at {0}: {1}")]
    ParseError(PathBuf, String),

    /// IO error reading config file
    #[error("Failed to read config at {0}: {1}")]
    IoError(PathBuf, std::io::Error),
}

/// Project-level configuration
///
/// Loaded from `.sqry-config.toml` with defaults for missing fields.
/// See `02_DESIGN.md` \[M3\] for full specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ProjectConfig {
    /// Ignore patterns configuration
    #[serde(default)]
    pub ignore: IgnoreConfig,

    /// Include patterns (override ignores)
    #[serde(default)]
    pub include: IncludeConfig,

    /// Language detection hints
    #[serde(default)]
    pub languages: LanguageConfig,

    /// Indexing behavior configuration
    #[serde(default)]
    pub indexing: IndexingConfig,

    /// Cache configuration
    #[serde(default)]
    pub cache: CacheConfig,
}

/// Ignore patterns configuration
///
/// These paths are excluded from indexing.
/// Uses gitignore-style syntax.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IgnoreConfig {
    /// Glob patterns to ignore (gitignore syntax)
    ///
    /// Default patterns include:
    /// - `node_modules/**`
    /// - `target/**`
    /// - `dist/**`
    /// - `*.min.js`
    /// - `vendor/**`
    /// - `.git/**`
    #[serde(default = "default_ignore_patterns")]
    pub patterns: Vec<String>,
}

impl Default for IgnoreConfig {
    fn default() -> Self {
        Self {
            patterns: default_ignore_patterns(),
        }
    }
}

fn default_ignore_patterns() -> Vec<String> {
    vec![
        "node_modules/**".to_string(),
        "target/**".to_string(),
        "dist/**".to_string(),
        "*.min.js".to_string(),
        "vendor/**".to_string(),
        ".git/**".to_string(),
        "__pycache__/**".to_string(),
        ".pytest_cache/**".to_string(),
        ".mypy_cache/**".to_string(),
        ".tox/**".to_string(),
        ".venv/**".to_string(),
        "venv/**".to_string(),
        ".gradle/**".to_string(),
        ".idea/**".to_string(),
        ".vs/**".to_string(),
        ".vscode/**".to_string(),
    ]
}

/// Include patterns (override ignores)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct IncludeConfig {
    /// Glob patterns to include even if they match ignore patterns
    ///
    /// Example: `vendor/internal/**` to include internal vendor code
    #[serde(default)]
    pub patterns: Vec<String>,
}

/// Language detection hints
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct LanguageConfig {
    /// Map file extensions to language IDs
    ///
    /// Example: `{ "jsx" = "javascript", "tsx" = "typescript" }`
    #[serde(default)]
    pub extensions: HashMap<String, String>,

    /// Map specific filenames to language IDs
    ///
    /// Example: `{ "Jenkinsfile" = "groovy", "Dockerfile.*" = "dockerfile" }`
    #[serde(default)]
    pub files: HashMap<String, String>,
}

/// Indexing behavior configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct IndexingConfig {
    /// Maximum file size to index (bytes)
    ///
    /// Default: 10 MB (`10_485_760` bytes)
    #[serde(default = "default_max_file_size")]
    pub max_file_size: u64,

    /// Maximum depth for directory traversal
    ///
    /// Default: 100
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,

    /// Enable scope extraction
    ///
    /// Default: true
    #[serde(default = "default_true")]
    pub enable_scope_extraction: bool,

    /// Enable relation extraction
    ///
    /// Default: true
    #[serde(default = "default_true")]
    pub enable_relation_extraction: bool,

    /// Custom directories to ignore during repo detection
    ///
    /// Extends the default ignored directories list.
    /// Default: empty (uses `DEFAULT_IGNORED_DIRS` from `path_utils`)
    #[serde(default)]
    pub additional_ignored_dirs: Vec<String>,
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            max_file_size: default_max_file_size(),
            max_depth: default_max_depth(),
            enable_scope_extraction: true,
            enable_relation_extraction: true,
            additional_ignored_dirs: Vec::new(),
        }
    }
}

fn default_max_file_size() -> u64 {
    10_485_760 // 10 MB
}

fn default_max_depth() -> u32 {
    100
}

fn default_true() -> bool {
    true
}

/// Cache configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CacheConfig {
    /// Directory for cache files (relative to `index_root`)
    ///
    /// Default: ".sqry-cache"
    #[serde(default = "default_cache_directory")]
    pub directory: String,

    /// Enable persistent cache
    ///
    /// Default: true
    #[serde(default = "default_true")]
    pub persistent: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            directory: default_cache_directory(),
            persistent: true,
        }
    }
}

fn default_cache_directory() -> String {
    ".sqry-cache".to_string()
}

impl ProjectConfig {
    /// Load configuration from a TOML file
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the config file
    ///
    /// # Errors
    ///
    /// Returns `ConfigError` if file cannot be read or parsed
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let path = path.as_ref();

        let contents = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::IoError(path.to_path_buf(), e))?;

        toml::from_str(&contents)
            .map_err(|e| ConfigError::ParseError(path.to_path_buf(), e.to_string()))
    }

    /// Load configuration by searching from `index_root` up to filesystem root
    ///
    /// Implements the search strategy from `02_DESIGN.md` \[M3\]:
    /// 1. Start at `index_root`
    /// 2. Check for `.sqry-config.toml`
    /// 3. If found: load and return
    /// 4. If not: move to parent and repeat
    /// 5. If reaching root without finding: return default config
    ///
    /// # Arguments
    ///
    /// * `index_root` - The project's index root path
    ///
    /// # Returns
    ///
    /// Configuration loaded from file or default.
    /// Errors are logged but return default config.
    pub fn load_from_index_root<P: AsRef<Path>>(index_root: P) -> Self {
        match Self::try_load_from_index_root(index_root.as_ref()) {
            Ok(config) => config,
            Err(ConfigError::NotFound) => {
                // Silent: use defaults
                Self::default()
            }
            Err(ConfigError::ParseError(path, err)) => {
                log::warn!(
                    "Malformed {} at {}: {}. Using defaults.",
                    CONFIG_FILE_NAME,
                    path.display(),
                    err
                );
                Self::default()
            }
            Err(ConfigError::IoError(path, err)) => {
                log::warn!(
                    "Cannot read {} at {}: {}. Using defaults.",
                    CONFIG_FILE_NAME,
                    path.display(),
                    err
                );
                Self::default()
            }
        }
    }

    /// Try to load configuration, returning error on failure
    ///
    /// Unlike `load_from_index_root`, this returns the error instead of
    /// falling back to defaults.
    fn try_load_from_index_root(index_root: &Path) -> Result<Self, ConfigError> {
        let mut current = index_root;

        loop {
            let config_path = current.join(CONFIG_FILE_NAME);

            if config_path.exists() {
                return Self::load(&config_path);
            }

            // Move to parent directory
            match current.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => {
                    current = parent;
                }
                _ => break, // Reached filesystem root
            }
        }

        Err(ConfigError::NotFound)
    }

    /// Get the effective ignored directories list
    ///
    /// Combines the default ignored directories with any additional
    /// directories specified in the configuration.
    #[must_use]
    pub fn effective_ignored_dirs(&self) -> Vec<&str> {
        use crate::project::path_utils::DEFAULT_IGNORED_DIRS;

        let mut dirs: Vec<&str> = DEFAULT_IGNORED_DIRS.to_vec();

        // Add any additional ignored dirs from config
        for dir in &self.indexing.additional_ignored_dirs {
            dirs.push(dir.as_str());
        }

        dirs
    }

    /// Check if a path matches any ignore pattern
    ///
    /// Returns true if the path should be ignored during indexing.
    /// Uses gitignore-style semantics:
    /// - Patterns like `node_modules/**` match anywhere in the path
    /// - Include patterns override ignore patterns
    ///
    /// # Arguments
    ///
    /// * `path` - Path to check (can be absolute or relative)
    ///
    /// # Returns
    ///
    /// `true` if the path should be ignored, `false` otherwise
    #[must_use]
    pub fn is_ignored(&self, path: &Path) -> bool {
        // Normalize path for consistent matching across platforms
        let normalized = normalize_path_for_matching(path);

        // Build ignore matcher (lazily - could be cached for performance)
        let ignore_set = match build_glob_set(&self.ignore.patterns) {
            Ok(set) => set,
            Err(e) => {
                log::warn!("Invalid ignore pattern: {e}");
                return false;
            }
        };

        // Check if path matches any ignore pattern
        if ignore_set.is_match(&normalized) {
            // Check if included (overrides ignore)
            if !self.include.patterns.is_empty() {
                let include_set = match build_glob_set(&self.include.patterns) {
                    Ok(set) => set,
                    Err(e) => {
                        log::warn!("Invalid include pattern: {e}");
                        return true; // Still ignored if include patterns are invalid
                    }
                };

                if include_set.is_match(&normalized) {
                    return false; // Include overrides ignore
                }
            }
            return true;
        }

        false
    }

    /// Get the language ID for a file path based on configuration hints
    ///
    /// Returns the configured language if found, or None to use default detection.
    #[must_use]
    pub fn language_for_path(&self, path: &Path) -> Option<&str> {
        // Check filename mappings first
        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
            for (pattern, lang) in &self.languages.files {
                if glob_match_filename(pattern, filename) {
                    return Some(lang.as_str());
                }
            }
        }

        // Check extension mappings
        if let Some(ext) = path.extension().and_then(|e| e.to_str())
            && let Some(lang) = self.languages.extensions.get(ext)
        {
            return Some(lang.as_str());
        }

        None
    }
}

/// Build a `GlobSet` from a list of patterns
///
/// For gitignore-style matching, patterns without a leading `/` are converted
/// to match anywhere in the path by prepending `**/`.
///
/// # Arguments
///
/// * `patterns` - List of glob patterns (gitignore syntax)
///
/// # Returns
///
/// A compiled `GlobSet` for efficient matching, or an error if patterns are invalid
fn build_glob_set(patterns: &[String]) -> Result<GlobSet, globset::Error> {
    let mut builder = GlobSetBuilder::new();

    for pattern in patterns {
        // Normalize pattern for gitignore-style matching (may expand to multiple patterns)
        for normalized in normalize_gitignore_pattern(pattern) {
            let glob = Glob::new(&normalized)?;
            builder.add(glob);
        }
    }

    builder.build()
}

/// Normalize a gitignore-style pattern for globset matching
///
/// Gitignore semantics:
/// - `/build` → rooted pattern, matches `build/` at root and all contents
/// - `docs/*.md` → contains `/`, so root-relative (NOT `**/docs/*.md`)
/// - `node_modules` → no `/`, matches anywhere in tree (`**/node_modules`)
/// - `*.bak` → no `/`, matches anywhere (`**/*.bak`)
/// - `build/` → trailing `/` only, directory anywhere (`**/build/**`)
///
/// Returns a Vec because directory patterns expand to multiple globs
/// (e.g., `/build` → `["build", "build/**"]` to match both entry and contents)
fn normalize_gitignore_pattern(pattern: &str) -> Vec<String> {
    // Handle rooted patterns (starting with `/`)
    if let Some(stripped) = pattern.strip_prefix('/') {
        return normalize_rooted_pattern(stripped);
    }

    // If pattern already starts with `**/`, use as-is
    if pattern.starts_with("**/") {
        return vec![pattern.to_string()];
    }

    // For checking root-relative patterns, we need to see if there's a path separator
    // EXCLUDING trailing `/**` or `/` which are special suffixes, not path separators
    // e.g., `node_modules/**` should match anywhere, but `docs/*.md` is root-relative
    let pattern_core = pattern
        .strip_suffix("/**")
        .or_else(|| pattern.strip_suffix('/'))
        .unwrap_or(pattern);

    // If the core pattern contains `/`, it's a path pattern (root-relative per gitignore)
    if pattern_core.contains('/') {
        // Root-relative pattern like `docs/*.md` or `src/build/`
        // Don't prepend `**/`
        if pattern.ends_with('/') && !pattern.ends_with("/**") {
            // Directory pattern with trailing `/` - match entry and contents
            let dir_name = pattern.trim_end_matches('/');
            return vec![dir_name.to_string(), format!("{dir_name}/**")];
        }
        return vec![pattern.to_string()];
    }

    // Simple pattern - matches anywhere in tree
    // Handle different suffixes:
    if pattern.ends_with("/**") {
        // `node_modules/**` → `**/node_modules/**`
        return vec![format!("**/{pattern}")];
    }

    if pattern.ends_with('/') {
        // Directory pattern like `build/` → match `**/build` and `**/build/**`
        let dir_name = pattern.trim_end_matches('/');
        return vec![format!("**/{dir_name}"), format!("**/{dir_name}/**")];
    }

    // Simple name or extension pattern
    vec![format!("**/{pattern}")]
}

/// Normalize a rooted pattern (one that started with `/`)
fn normalize_rooted_pattern(pattern: &str) -> Vec<String> {
    // Already has `/**` suffix - use as-is
    if pattern.ends_with("/**") {
        return vec![pattern.to_string()];
    }

    // Has trailing `/` - it's explicitly a directory, match entry and contents
    if pattern.ends_with('/') {
        let dir_name = pattern.trim_end_matches('/');
        return vec![dir_name.to_string(), format!("{dir_name}/**")];
    }

    // Check if it looks like a file vs a directory
    // - Glob patterns with `*`, `?`, `[` are likely file patterns
    // - Names with a file extension (e.g., `.json`, `.txt`) are files
    // - Hidden directories (starting with `.`) like `.git`, `.sqry-cache` are directories
    let last_segment = pattern.rsplit(['/', '\\']).next().unwrap_or(pattern);

    // Check for glob wildcards - treat as file pattern
    let has_glob =
        last_segment.contains('*') || last_segment.contains('?') || last_segment.contains('[');

    if has_glob {
        return vec![pattern.to_string()];
    }

    // Check for file extension - determine if this looks like a file or directory
    // - Files have extensions: `config.json`, `.env.local`, `.gitignore`
    // - Directories are simple names or hidden: `.git`, `.vscode`, `.sqry-cache`
    //
    // For hidden names (starting with `.`):
    // - Check for common dotfile suffixes that indicate files
    // - Otherwise assume directory
    let looks_like_file = if let Some(name) = last_segment.strip_prefix('.') {
        // Hidden name - check for known dotfile patterns that are files
        // Use suffix matches with special-case for .config directory (iter5 fix)
        //
        // Files: .gitignore, .gitattributes, .editorconfig, .gitconfig, .eslintrc, .prettierrc
        // Directories: .git, .svn, .vscode, .cache, .config, .sqry-cache, .npm
        name.ends_with("ignore") // .gitignore, .dockerignore, .eslintignore
            || name.ends_with("rc") // .eslintrc, .prettierrc, .bashrc
            || name.ends_with("attributes") // .gitattributes
            || name.ends_with("modules") // .gitmodules
            // *config files (except .config directory itself)
            // .gitconfig, .editorconfig → file; .config → directory
            || (name != "config" && name.ends_with("config"))
            || name.contains('.') // .env.local, .npmrc.bak have inner dot
    } else if let Some(dot_pos) = last_segment.rfind('.') {
        // Regular name with dot - check if it's an extension
        dot_pos > 0 && dot_pos < last_segment.len() - 1
    } else {
        false
    };

    if looks_like_file {
        // File pattern - just return as-is
        vec![pattern.to_string()]
    } else {
        // Directory pattern (including hidden dirs like .git) - return both entry and contents
        vec![pattern.to_string(), format!("{pattern}/**")]
    }
}

/// Simple filename-only glob matching for language configuration
///
/// Uses globset for proper glob semantics on just the filename component.
fn glob_match_filename(pattern: &str, filename: &str) -> bool {
    // For simple extension patterns like `*.jsx`, match against filename
    match Glob::new(pattern) {
        Ok(glob) => glob.compile_matcher().is_match(filename),
        Err(_) => pattern == filename,
    }
}

/// Normalize a path to use forward slashes and strip leading `/`
///
/// This ensures consistent matching across platforms and allows unanchored patterns
/// to match absolute paths. The leading `/` is stripped so that patterns like
/// `**/node_modules/**` can match `/home/user/project/node_modules/foo`.
fn normalize_path_for_matching(path: &Path) -> String {
    // Convert to string with forward slashes
    let path_str = path.to_string_lossy();

    // Normalize path separators (Windows uses `\`)
    let normalized = path_str.replace('\\', "/");

    // Strip leading `/` from absolute paths for consistent matching
    normalized
        .strip_prefix('/')
        .unwrap_or(&normalized)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = ProjectConfig::default();

        // Check defaults
        assert!(!config.ignore.patterns.is_empty());
        assert!(config.include.patterns.is_empty());
        assert!(config.languages.extensions.is_empty());
        assert_eq!(config.indexing.max_file_size, 10_485_760);
        assert_eq!(config.indexing.max_depth, 100);
        assert!(config.indexing.enable_scope_extraction);
        assert!(config.indexing.enable_relation_extraction);
        assert_eq!(config.cache.directory, ".sqry-cache");
        assert!(config.cache.persistent);
    }

    #[test]
    fn test_load_config_from_file() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join(CONFIG_FILE_NAME);

        let toml_content = r#"
[ignore]
patterns = ["custom/**", "*.bak"]

[include]
patterns = ["custom/important/**"]

[languages]
extensions = { "jsx" = "javascript" }
files = { "Jenkinsfile" = "groovy" }

[indexing]
max_file_size = 5242880
max_depth = 50
enable_scope_extraction = false
additional_ignored_dirs = ["my_vendor"]

[cache]
directory = ".my-cache"
persistent = false
"#;

        std::fs::write(&config_path, toml_content).unwrap();

        let config = ProjectConfig::load(&config_path).unwrap();

        assert_eq!(config.ignore.patterns, vec!["custom/**", "*.bak"]);
        assert_eq!(config.include.patterns, vec!["custom/important/**"]);
        assert_eq!(
            config.languages.extensions.get("jsx"),
            Some(&"javascript".to_string())
        );
        assert_eq!(
            config.languages.files.get("Jenkinsfile"),
            Some(&"groovy".to_string())
        );
        assert_eq!(config.indexing.max_file_size, 5_242_880);
        assert_eq!(config.indexing.max_depth, 50);
        assert!(!config.indexing.enable_scope_extraction);
        assert_eq!(config.indexing.additional_ignored_dirs, vec!["my_vendor"]);
        assert_eq!(config.cache.directory, ".my-cache");
        assert!(!config.cache.persistent);
    }

    #[test]
    fn test_load_config_ancestor_walk() {
        let temp = TempDir::new().unwrap();

        // Create nested directory structure
        let nested = temp.path().join("level1/level2/level3");
        std::fs::create_dir_all(&nested).unwrap();

        // Create config at level1
        let config_path = temp.path().join("level1").join(CONFIG_FILE_NAME);
        std::fs::write(
            &config_path,
            r"
[indexing]
max_depth = 42
",
        )
        .unwrap();

        // Load from level3 - should find config at level1
        let config = ProjectConfig::load_from_index_root(&nested);
        assert_eq!(config.indexing.max_depth, 42);
    }

    #[test]
    fn test_load_config_not_found_uses_defaults() {
        let temp = TempDir::new().unwrap();

        // No config file exists
        let config = ProjectConfig::load_from_index_root(temp.path());

        // Should use defaults
        assert_eq!(config, ProjectConfig::default());
    }

    #[test]
    fn test_partial_config_uses_defaults() {
        let temp = TempDir::new().unwrap();
        let config_path = temp.path().join(CONFIG_FILE_NAME);

        // Only specify indexing section
        std::fs::write(
            &config_path,
            r"
[indexing]
max_depth = 25
",
        )
        .unwrap();

        let config = ProjectConfig::load(&config_path).unwrap();

        // Specified value
        assert_eq!(config.indexing.max_depth, 25);

        // Defaults for unspecified
        assert_eq!(config.indexing.max_file_size, 10_485_760);
        assert!(config.indexing.enable_scope_extraction);
        assert_eq!(config.cache.directory, ".sqry-cache");
    }

    #[test]
    fn test_effective_ignored_dirs() {
        let mut config = ProjectConfig::default();
        config.indexing.additional_ignored_dirs =
            vec!["my_vendor".to_string(), "artifacts".to_string()];

        let dirs = config.effective_ignored_dirs();

        // Should include defaults
        assert!(dirs.contains(&"node_modules"));
        assert!(dirs.contains(&"target"));

        // Should include custom
        assert!(dirs.contains(&"my_vendor"));
        assert!(dirs.contains(&"artifacts"));
    }

    #[test]
    fn test_language_for_path() {
        let mut config = ProjectConfig::default();
        config
            .languages
            .extensions
            .insert("jsx".to_string(), "javascript".to_string());
        config
            .languages
            .files
            .insert("Jenkinsfile".to_string(), "groovy".to_string());

        // Extension match
        assert_eq!(
            config.language_for_path(Path::new("src/App.jsx")),
            Some("javascript")
        );

        // Filename match
        assert_eq!(
            config.language_for_path(Path::new("ci/Jenkinsfile")),
            Some("groovy")
        );

        // No match
        assert_eq!(config.language_for_path(Path::new("src/main.rs")), None);
    }

    #[test]
    fn test_glob_match_filename() {
        // Single wildcard
        assert!(glob_match_filename("*.js", "app.js"));
        assert!(!glob_match_filename("*.js", "app.ts"));

        // Question mark
        assert!(glob_match_filename("file?.txt", "file1.txt"));
        assert!(!glob_match_filename("file?.txt", "file12.txt"));

        // Exact match
        assert!(glob_match_filename("Jenkinsfile", "Jenkinsfile"));
        assert!(!glob_match_filename("Jenkinsfile", "Jenkinsfile.bak"));
    }

    #[test]
    fn test_is_ignored_basic() {
        let config = ProjectConfig::default();

        // Default patterns should ignore common dirs
        assert!(config.is_ignored(Path::new("node_modules/foo.js")));
        assert!(config.is_ignored(Path::new("target/debug/binary")));
        assert!(config.is_ignored(Path::new(".git/config")));
        assert!(config.is_ignored(Path::new("__pycache__/module.pyc")));

        // Non-ignored paths
        assert!(!config.is_ignored(Path::new("src/main.rs")));
        assert!(!config.is_ignored(Path::new("lib/utils.js")));
    }

    #[test]
    fn test_is_ignored_nested_paths() {
        // Key fix: patterns like `node_modules/**` should match nested paths
        let config = ProjectConfig::default();

        // Nested node_modules should be ignored
        assert!(config.is_ignored(Path::new("packages/frontend/node_modules/react/index.js")));
        assert!(config.is_ignored(Path::new("deep/nested/path/node_modules/pkg/lib.js")));

        // Nested target dirs should be ignored
        assert!(config.is_ignored(Path::new("crates/lib/target/release/libfoo.so")));
    }

    #[test]
    fn test_is_ignored_absolute_paths() {
        // Key fix: patterns should match absolute paths too
        let config = ProjectConfig::default();

        // Absolute paths with ignored directories
        assert!(config.is_ignored(Path::new("/home/user/project/node_modules/pkg/index.js")));
        assert!(config.is_ignored(Path::new("/tmp/build/target/debug/app")));
        assert!(config.is_ignored(Path::new("/var/repo/.git/objects/pack/abc")));
    }

    #[test]
    fn test_is_ignored_include_overrides() {
        let mut config = ProjectConfig::default();
        config.ignore.patterns = vec!["vendor/**".to_string()];
        config.include.patterns = vec!["vendor/internal/**".to_string()];

        // vendor/** should be ignored
        assert!(config.is_ignored(Path::new("vendor/external/lib.js")));
        assert!(config.is_ignored(Path::new("vendor/third_party/pkg.py")));

        // But vendor/internal/** should be included (override)
        assert!(!config.is_ignored(Path::new("vendor/internal/core.rs")));
        assert!(!config.is_ignored(Path::new("vendor/internal/nested/utils.rs")));
    }

    #[test]
    fn test_is_ignored_extension_patterns() {
        let mut config = ProjectConfig::default();
        config.ignore.patterns = vec!["*.min.js".to_string(), "*.bak".to_string()];

        // Extension patterns should work
        assert!(config.is_ignored(Path::new("dist/app.min.js")));
        assert!(config.is_ignored(Path::new("src/old.bak")));
        assert!(config.is_ignored(Path::new("deeply/nested/file.min.js")));

        // Normal files shouldn't be ignored
        assert!(!config.is_ignored(Path::new("src/app.js")));
    }

    #[test]
    fn test_normalize_gitignore_pattern() {
        // Simple patterns without `/` get `**/` prepended (match anywhere)
        assert_eq!(
            normalize_gitignore_pattern("node_modules/**"),
            vec!["**/node_modules/**"]
        );
        assert_eq!(normalize_gitignore_pattern("*.js"), vec!["**/*.js"]);
        assert_eq!(normalize_gitignore_pattern("target"), vec!["**/target"]);

        // Patterns starting with `**/` are unchanged
        assert_eq!(
            normalize_gitignore_pattern("**/node_modules"),
            vec!["**/node_modules"]
        );

        // Rooted patterns (`/build`) expand to entry + contents (fixes HIGH finding)
        assert_eq!(
            normalize_gitignore_pattern("/build"),
            vec!["build", "build/**"]
        );
        // Rooted patterns with `/**` already - just strip the `/`
        assert_eq!(normalize_gitignore_pattern("/dist/**"), vec!["dist/**"]);

        // Rooted file patterns (have `.`) don't get `/**` appended
        assert_eq!(
            normalize_gitignore_pattern("/config.json"),
            vec!["config.json"]
        );
        assert_eq!(normalize_gitignore_pattern("/*.txt"), vec!["*.txt"]);

        // Rooted directory with trailing `/` expands to entry + contents
        assert_eq!(
            normalize_gitignore_pattern("/build/"),
            vec!["build", "build/**"]
        );

        // Patterns with `/` (not leading) are root-relative - NO `**/` prefix (fixes MEDIUM finding)
        assert_eq!(normalize_gitignore_pattern("docs/*.md"), vec!["docs/*.md"]);
        assert_eq!(
            normalize_gitignore_pattern("src/vendor"),
            vec!["src/vendor"]
        );

        // Directory patterns with trailing `/` only - match anywhere
        assert_eq!(
            normalize_gitignore_pattern("build/"),
            vec!["**/build", "**/build/**"]
        );
    }

    #[test]
    fn test_build_glob_set() {
        let patterns = vec!["node_modules/**".to_string(), "*.min.js".to_string()];
        let glob_set = build_glob_set(&patterns).unwrap();

        // Should match nested paths
        assert!(glob_set.is_match("src/node_modules/pkg/index.js"));
        assert!(glob_set.is_match("app.min.js"));
        assert!(glob_set.is_match("dist/bundle.min.js"));

        // Should not match non-matching paths
        assert!(!glob_set.is_match("src/main.rs"));
        assert!(!glob_set.is_match("app.js"));
    }

    #[test]
    fn test_rooted_pattern_matches_contents() {
        // Test for HIGH finding: `/build` should match `build/output.log`
        let patterns = vec!["/build".to_string()];
        let glob_set = build_glob_set(&patterns).unwrap();

        // Should match the directory itself
        assert!(glob_set.is_match("build"));
        // Should match contents of the directory
        assert!(glob_set.is_match("build/output.log"));
        assert!(glob_set.is_match("build/subdir/file.txt"));

        // Should NOT match elsewhere (rooted pattern)
        assert!(!glob_set.is_match("src/build/output.log"));
        assert!(!glob_set.is_match("packages/build"));
    }

    #[test]
    fn test_slash_containing_patterns_are_root_relative() {
        // Test for MEDIUM finding: `docs/*.md` should NOT match `packages/foo/docs/readme.md`
        let patterns = vec!["docs/*.md".to_string()];
        let glob_set = build_glob_set(&patterns).unwrap();

        // Should match at root
        assert!(glob_set.is_match("docs/readme.md"));
        assert!(glob_set.is_match("docs/api.md"));

        // Should NOT match in nested paths (false positives)
        assert!(!glob_set.is_match("packages/foo/docs/readme.md"));
        assert!(!glob_set.is_match("src/docs/notes.md"));
    }

    #[test]
    fn test_simple_patterns_match_anywhere() {
        // Simple patterns (no `/`) should match anywhere
        let patterns = vec!["*.bak".to_string(), "node_modules".to_string()];
        let glob_set = build_glob_set(&patterns).unwrap();

        // *.bak matches anywhere
        assert!(glob_set.is_match("file.bak"));
        assert!(glob_set.is_match("src/file.bak"));
        assert!(glob_set.is_match("deep/nested/path/file.bak"));

        // node_modules matches anywhere
        assert!(glob_set.is_match("node_modules"));
        assert!(glob_set.is_match("packages/frontend/node_modules"));
    }

    #[test]
    fn test_dotted_directories_expand_to_contents() {
        // Test for iter3 MEDIUM finding: `.git`, `.sqry-cache` should be treated as directories
        // not files, and expand to include contents

        // /.git should match both the directory and its contents
        assert_eq!(
            normalize_gitignore_pattern("/.git"),
            vec![".git", ".git/**"]
        );
        assert_eq!(
            normalize_gitignore_pattern("/.sqry-cache"),
            vec![".sqry-cache", ".sqry-cache/**"]
        );
        assert_eq!(
            normalize_gitignore_pattern("/.hidden"),
            vec![".hidden", ".hidden/**"]
        );
        // .config is XDG config directory, should expand (iter4 fix)
        assert_eq!(
            normalize_gitignore_pattern("/.config"),
            vec![".config", ".config/**"]
        );

        // *config dotfiles are files, not directories (iter5 fix)
        // .gitconfig, .editorconfig → file; .config → directory
        assert_eq!(
            normalize_gitignore_pattern("/.gitconfig"),
            vec![".gitconfig"]
        );
        assert_eq!(
            normalize_gitignore_pattern("/.editorconfig"),
            vec![".editorconfig"]
        );

        // But files with actual extensions should NOT expand
        assert_eq!(
            normalize_gitignore_pattern("/.gitignore"),
            vec![".gitignore"]
        );
        assert_eq!(
            normalize_gitignore_pattern("/config.json"),
            vec!["config.json"]
        );
        assert_eq!(
            normalize_gitignore_pattern("/.env.local"),
            vec![".env.local"]
        );

        // Verify via GlobSet that .git contents are matched
        let patterns = vec!["/.git".to_string()];
        let glob_set = build_glob_set(&patterns).unwrap();

        assert!(glob_set.is_match(".git"));
        assert!(glob_set.is_match(".git/config"));
        assert!(glob_set.is_match(".git/objects/pack/abc123"));
        assert!(glob_set.is_match(".git/refs/heads/main"));

        // But nested .git should NOT match (rooted pattern)
        assert!(!glob_set.is_match("submodule/.git"));
        assert!(!glob_set.is_match("packages/sub/.git/config"));
    }

    #[test]
    fn test_rooted_patterns_with_relative_paths() {
        // Rooted patterns like `/build` work with relative paths (relative to project root)
        // For absolute paths, project root context would be needed to strip the prefix first

        let mut config = ProjectConfig::default();
        config.ignore.patterns = vec!["/build".to_string(), "/.git".to_string()];

        // Relative paths work correctly with rooted patterns
        assert!(config.is_ignored(Path::new("build/output.log")));
        assert!(config.is_ignored(Path::new("build")));
        assert!(config.is_ignored(Path::new(".git/config")));
        assert!(config.is_ignored(Path::new(".git")));

        // Rooted patterns should NOT match in nested locations
        assert!(!config.is_ignored(Path::new("src/build/output.log")));
        assert!(!config.is_ignored(Path::new("packages/sub/build")));
        assert!(!config.is_ignored(Path::new("submodule/.git")));
    }

    #[test]
    fn test_unrooted_patterns_with_absolute_paths() {
        // Unrooted patterns (no leading `/`) match anywhere, including absolute paths
        // The leading `/` from absolute paths is stripped for matching

        let config = ProjectConfig::default();
        // Default patterns include `node_modules/**`, `.git/**`, etc. (unrooted)

        // Absolute paths work because leading `/` is stripped and patterns have `**/`
        assert!(config.is_ignored(Path::new("/home/user/project/node_modules/pkg/index.js")));
        assert!(config.is_ignored(Path::new("/tmp/build/target/debug/app")));
        assert!(config.is_ignored(Path::new("/var/repo/.git/objects/pack/abc")));

        // Relative paths also work
        assert!(config.is_ignored(Path::new("node_modules/pkg/index.js")));
        assert!(config.is_ignored(Path::new("target/debug/app")));
    }

    #[test]
    fn test_normalize_path_for_matching() {
        // Verify the path normalization strips leading `/` and normalizes separators
        assert_eq!(
            normalize_path_for_matching(Path::new("/home/user/project/src/main.rs")),
            "home/user/project/src/main.rs"
        );
        assert_eq!(
            normalize_path_for_matching(Path::new("relative/path/file.rs")),
            "relative/path/file.rs"
        );
        assert_eq!(normalize_path_for_matching(Path::new("/build")), "build");
    }
}
