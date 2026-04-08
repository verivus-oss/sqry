/// Query classification for semantic/text/hybrid search modes
pub mod classifier;
/// Fallback search (AST → ripgrep) - distinct from embedding-based hybrid search
pub mod fallback;
/// Fuzzy search implementation
pub mod fuzzy;
/// Pattern matching utilities
pub mod matcher;
/// Result ranking and relevance scoring
pub mod ranking;
/// SIMD-accelerated search operations
pub mod simd;
/// Trigram-based indexing
pub mod trigram;

use anyhow::{Context, Result};
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher as GrepSearcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{
    WalkBuilder,
    overrides::{Override, OverrideBuilder},
};
use std::io;
use std::path::{Path, PathBuf};

/// Error message for `SearchMode` variants unsupported by the ripgrep text searcher.
/// Semantic search is handled by `FallbackSearchEngine` / `QueryExecutor`;
/// fuzzy search is handled by `CandidateGenerator` + `FuzzyMatcher`.
const UNSUPPORTED_TEXT_SEARCHER_MODE: &str = "is not supported by the text searcher. Use FallbackSearchEngine for semantic search or CandidateGenerator for fuzzy search.";

/// Search mode determines how the search is performed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// Basic text search
    Text,
    /// Regex pattern search
    Regex,
    /// AST-aware semantic search
    Semantic,
    /// Fuzzy matching search
    Fuzzy,
}

/// A search match
#[derive(Debug, Clone, serde::Serialize)]
pub struct Match {
    /// Path to the file containing the match
    pub path: PathBuf,
    /// 1-based line number (compatible with grep output)
    pub line: u32,
    /// Text content of the matching line
    pub line_text: String,
    /// Byte position from start of file where this line begins
    pub byte_offset: usize,
}

/// Search configuration
#[derive(Debug, Clone)]
pub struct SearchConfig {
    /// Search mode (text, regex, semantic, fuzzy)
    pub mode: SearchMode,
    /// Whether to perform case-insensitive search
    pub case_insensitive: bool,
    /// Whether to include hidden files
    pub include_hidden: bool,
    /// Whether to follow symbolic links
    pub follow_symlinks: bool,
    /// Maximum directory depth for recursion
    pub max_depth: Option<usize>,
    /// File extensions to include (e.g., "rs", "js")
    pub file_types: Vec<String>,
    /// Patterns to exclude from search
    pub exclude_patterns: Vec<String>,
    /// Number of lines to show before each match
    pub before_context: usize,
    /// Number of lines to show after each match
    pub after_context: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            mode: SearchMode::Regex,
            case_insensitive: false,
            include_hidden: false,
            follow_symlinks: false,
            max_depth: None,
            file_types: Vec::new(),
            exclude_patterns: Vec::new(),
            before_context: 2,
            after_context: 2,
        }
    }
}

/// Main searcher using ripgrep's engine
pub struct Searcher {
    searcher: grep_searcher::Searcher,
}

/// Custom sink to collect matches with byte offsets
struct MatchSink<'a> {
    path: &'a Path,
    matches: Vec<Match>,
}

impl<'a> MatchSink<'a> {
    fn new(path: &'a Path) -> Self {
        Self {
            path,
            matches: Vec::new(),
        }
    }

    fn into_matches(self) -> Vec<Match> {
        self.matches
    }
}

impl Sink for MatchSink<'_> {
    type Error = io::Error;

    fn matched(
        &mut self,
        _searcher: &GrepSearcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, io::Error> {
        let line_text = String::from_utf8_lossy(mat.bytes()).to_string();
        // Line numbers beyond u32::MAX are impractical; clamp to max
        let line_number = mat
            .line_number()
            .unwrap_or(1)
            .min(u64::from(u32::MAX))
            .try_into()
            .unwrap_or(u32::MAX);
        // Byte offsets must fit in usize (architecture-dependent)
        let byte_offset = mat.absolute_byte_offset().try_into().unwrap_or(usize::MAX);

        self.matches.push(Match {
            path: self.path.to_path_buf(),
            line: line_number,
            line_text,
            byte_offset,
        });

        Ok(true)
    }
}

impl Searcher {
    /// Create a new searcher instance
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] if the underlying ripgrep searcher fails to initialise.
    pub fn new() -> Result<Self> {
        let searcher = SearcherBuilder::new()
            .binary_detection(BinaryDetection::quit(0))
            .line_number(true)
            .build();

        Ok(Self { searcher })
    }

    /// Search for a pattern in the given paths
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when walker construction fails, when a file cannot be read,
    /// or when the requested search mode is unsupported.
    pub fn search<P: AsRef<Path>>(
        &self,
        pattern: &str,
        paths: &[P],
        config: &SearchConfig,
    ) -> Result<Vec<Match>> {
        let mut all_matches = Vec::new();
        let matcher = Self::build_matcher(pattern, config)?;

        for path in paths {
            let path_matches = self.search_path(&matcher, path.as_ref(), config)?;
            all_matches.extend(path_matches);
        }

        Ok(all_matches)
    }

    fn build_matcher(pattern: &str, config: &SearchConfig) -> Result<RegexMatcher> {
        let mut matcher_builder = grep_regex::RegexMatcherBuilder::new();
        matcher_builder.case_insensitive(config.case_insensitive);

        let pattern_to_use = Self::pattern_for_mode(pattern, config.mode)?;

        matcher_builder.build(&pattern_to_use).map_err(Into::into)
    }

    fn pattern_for_mode(pattern: &str, mode: SearchMode) -> Result<String> {
        match mode {
            SearchMode::Text => Ok(regex::escape(pattern)),
            SearchMode::Regex => Ok(pattern.to_string()),
            SearchMode::Semantic | SearchMode::Fuzzy => Err(anyhow::anyhow!(
                "SearchMode::{mode:?} {UNSUPPORTED_TEXT_SEARCHER_MODE}"
            )),
        }
    }

    fn search_path(
        &self,
        matcher: &RegexMatcher,
        path: &Path,
        config: &SearchConfig,
    ) -> Result<Vec<Match>> {
        let walker = Self::build_walker(path, config)?;
        let mut match_results = Vec::new();

        for entry in walker {
            let entry = entry?;
            if !Self::is_searchable_entry(&entry, config) {
                continue;
            }

            let path = entry.path();
            let file_matches = self
                .search_file(matcher, path)
                .with_context(|| format!("Failed to search file: {}", path.display()))?;
            match_results.extend(file_matches);
        }

        Ok(match_results)
    }

    fn matches_file_type(path: &Path, config: &SearchConfig) -> bool {
        if config.file_types.is_empty() {
            return true;
        }

        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };

        config.file_types.iter().any(|candidate| candidate == ext)
    }

    /// Build a file walker with the given configuration
    fn build_walker(path: &Path, config: &SearchConfig) -> Result<ignore::Walk> {
        let mut builder = WalkBuilder::new(path);

        Self::configure_walker(&mut builder, config);
        Self::apply_exclude_overrides(&mut builder, path, &config.exclude_patterns)?;

        Ok(builder.build())
    }

    fn configure_walker(builder: &mut WalkBuilder, config: &SearchConfig) {
        builder
            .hidden(!config.include_hidden)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .follow_links(config.follow_symlinks);

        if let Some(max_depth) = config.max_depth {
            builder.max_depth(Some(max_depth));
        }
    }

    fn apply_exclude_overrides(
        builder: &mut WalkBuilder,
        path: &Path,
        exclude_patterns: &[String],
    ) -> Result<()> {
        if exclude_patterns.is_empty() {
            return Ok(());
        }

        let overrides = Self::build_exclude_overrides(path, exclude_patterns)?;
        builder.overrides(overrides);
        Ok(())
    }

    fn build_exclude_overrides(path: &Path, exclude_patterns: &[String]) -> Result<Override> {
        // Build exclude patterns using OverrideBuilder
        //
        // Note on gitignore syntax: The `ignore` crate's OverrideBuilder requires `!` prefix
        // for exclusions (e.g., `!*.test.js`). This differs from standard gitignore semantics
        // where patterns without `!` are exclusions by default. Here, we automatically prepend
        // `!` to user-provided patterns to match expected gitignore behavior.
        //
        // Example transformations:
        //   User input: "*.min.js"     -> OverrideBuilder: "!*.min.js" (exclude minified files)
        //   User input: "target/**"    -> OverrideBuilder: "!target/**" (exclude target directory)
        //   User input: "node_modules" -> OverrideBuilder: "!node_modules" (exclude node_modules)
        let mut override_builder = OverrideBuilder::new(path);
        for pattern in exclude_patterns {
            // Patterns should be in gitignore format (e.g., "*.min.js", "target/**")
            override_builder
                .add(&format!("!{pattern}"))
                .with_context(|| format!("Invalid exclude pattern: {pattern}"))?;
        }
        override_builder
            .build()
            .context("Failed to build exclude overrides")
    }

    fn is_searchable_entry(entry: &ignore::DirEntry, config: &SearchConfig) -> bool {
        let path = entry.path();
        path.is_file() && Self::matches_file_type(path, config)
    }

    /// Search a single file
    fn search_file(&self, matcher: &RegexMatcher, path: &Path) -> Result<Vec<Match>> {
        let mut searcher = self.searcher.clone();
        let mut sink = MatchSink::new(path);

        searcher
            .search_path(matcher, path, &mut sink)
            .map_err(|e| anyhow::anyhow!("Search failed: {e}"))?;

        Ok(sink.into_matches())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // SearchMode tests
    #[test]
    fn test_search_mode_equality() {
        assert_eq!(SearchMode::Text, SearchMode::Text);
        assert_eq!(SearchMode::Regex, SearchMode::Regex);
        assert_eq!(SearchMode::Semantic, SearchMode::Semantic);
        assert_eq!(SearchMode::Fuzzy, SearchMode::Fuzzy);
    }

    #[test]
    fn test_search_mode_inequality() {
        assert_ne!(SearchMode::Text, SearchMode::Regex);
        assert_ne!(SearchMode::Semantic, SearchMode::Fuzzy);
    }

    #[test]
    fn test_search_mode_clone() {
        let mode = SearchMode::Regex;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }

    #[test]
    fn test_search_mode_debug() {
        let debug = format!("{:?}", SearchMode::Text);
        assert!(debug.contains("Text"));
    }

    // Match tests
    #[test]
    fn test_match_creation() {
        let m = Match {
            path: PathBuf::from("test.rs"),
            line: 42,
            line_text: "fn test() {}".to_string(),
            byte_offset: 100,
        };

        assert_eq!(m.path, PathBuf::from("test.rs"));
        assert_eq!(m.line, 42);
        assert_eq!(m.line_text, "fn test() {}");
        assert_eq!(m.byte_offset, 100);
    }

    #[test]
    fn test_match_clone() {
        let m = Match {
            path: PathBuf::from("test.rs"),
            line: 1,
            line_text: "test".to_string(),
            byte_offset: 0,
        };

        let cloned = m.clone();
        assert_eq!(m.path, cloned.path);
        assert_eq!(m.line, cloned.line);
    }

    #[test]
    fn test_match_debug() {
        let m = Match {
            path: PathBuf::from("test.rs"),
            line: 1,
            line_text: "test".to_string(),
            byte_offset: 0,
        };

        let debug = format!("{m:?}");
        assert!(debug.contains("Match"));
        assert!(debug.contains("test.rs"));
    }

    #[test]
    fn test_match_serialize() {
        let m = Match {
            path: PathBuf::from("test.rs"),
            line: 10,
            line_text: "hello".to_string(),
            byte_offset: 50,
        };

        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("test.rs"));
        assert!(json.contains("hello"));
        assert!(json.contains("10"));
    }

    // SearchConfig tests
    #[test]
    fn test_search_config_default() {
        let config = SearchConfig::default();

        assert_eq!(config.mode, SearchMode::Regex);
        assert!(!config.case_insensitive);
        assert!(!config.include_hidden);
        assert!(!config.follow_symlinks);
        assert!(config.max_depth.is_none());
        assert!(config.file_types.is_empty());
        assert!(config.exclude_patterns.is_empty());
        assert_eq!(config.before_context, 2);
        assert_eq!(config.after_context, 2);
    }

    #[test]
    fn test_search_config_custom() {
        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: true,
            include_hidden: true,
            follow_symlinks: true,
            max_depth: Some(5),
            file_types: vec!["rs".to_string(), "js".to_string()],
            exclude_patterns: vec!["*.min.js".to_string()],
            before_context: 3,
            after_context: 3,
        };

        assert_eq!(config.mode, SearchMode::Text);
        assert!(config.case_insensitive);
        assert!(config.include_hidden);
        assert!(config.follow_symlinks);
        assert_eq!(config.max_depth, Some(5));
        assert_eq!(config.file_types.len(), 2);
        assert_eq!(config.exclude_patterns.len(), 1);
    }

    #[test]
    fn test_search_config_clone() {
        let config = SearchConfig {
            mode: SearchMode::Fuzzy,
            case_insensitive: true,
            ..Default::default()
        };

        let cloned = config.clone();
        assert_eq!(config.mode, cloned.mode);
        assert_eq!(config.case_insensitive, cloned.case_insensitive);
    }

    #[test]
    fn test_search_config_debug() {
        let config = SearchConfig::default();
        let debug = format!("{config:?}");
        assert!(debug.contains("SearchConfig"));
        assert!(debug.contains("mode"));
    }

    // Searcher tests
    #[test]
    fn test_searcher_new() {
        let searcher = Searcher::new();
        assert!(searcher.is_ok());
    }

    #[test]
    fn test_searcher_text_search() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join("test.rs");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "fn main() {{").unwrap();
        writeln!(file, "    println!(\"hello world\");").unwrap();
        writeln!(file, "}}").unwrap();
        drop(file);

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            mode: SearchMode::Text,
            ..Default::default()
        };

        let matches = searcher
            .search("hello", &[tmp_dir.path()], &config)
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert!(matches[0].line_text.contains("hello world"));
        assert_eq!(matches[0].line, 2);
    }

    #[test]
    fn test_searcher_regex_search() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join("test.rs");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "let x = 123;").unwrap();
        writeln!(file, "let y = 456;").unwrap();
        writeln!(file, "let z = abc;").unwrap();
        drop(file);

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            mode: SearchMode::Regex,
            ..Default::default()
        };

        // Match lines with numbers
        let matches = searcher.search(r"\d+", &[tmp_dir.path()], &config).unwrap();

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_searcher_case_insensitive() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join("test.txt");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "Hello World").unwrap();
        writeln!(file, "HELLO WORLD").unwrap();
        writeln!(file, "hello world").unwrap();
        drop(file);

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            mode: SearchMode::Text,
            case_insensitive: true,
            ..Default::default()
        };

        let matches = searcher
            .search("hello", &[tmp_dir.path()], &config)
            .unwrap();

        assert_eq!(matches.len(), 3);
    }

    #[test]
    fn test_searcher_file_type_filter() {
        let tmp_dir = TempDir::new().unwrap();

        let rs_file = tmp_dir.path().join("test.rs");
        std::fs::write(&rs_file, "fn test() {}").unwrap();

        let js_file = tmp_dir.path().join("test.js");
        std::fs::write(&js_file, "function test() {}").unwrap();

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            mode: SearchMode::Text,
            file_types: vec!["rs".to_string()],
            ..Default::default()
        };

        let matches = searcher.search("test", &[tmp_dir.path()], &config).unwrap();

        // Should only match .rs file
        assert_eq!(matches.len(), 1);
        assert!(matches[0].path.to_string_lossy().ends_with(".rs"));
    }

    #[test]
    fn test_searcher_no_matches() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig::default();

        let matches = searcher
            .search("nonexistent_pattern_xyz", &[tmp_dir.path()], &config)
            .unwrap();

        assert!(matches.is_empty());
    }

    #[test]
    fn test_searcher_semantic_mode_unsupported() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            mode: SearchMode::Semantic,
            ..Default::default()
        };

        let result = searcher.search("test", &[tmp_dir.path()], &config);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Semantic"));
        assert!(err_msg.contains("not supported by the text searcher"));
    }

    #[test]
    fn test_searcher_fuzzy_mode_unsupported() {
        let tmp_dir = TempDir::new().unwrap();
        let file_path = tmp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {}").unwrap();

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            mode: SearchMode::Fuzzy,
            ..Default::default()
        };

        let result = searcher.search("test", &[tmp_dir.path()], &config);

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Fuzzy"));
        assert!(err_msg.contains("not supported by the text searcher"));
    }

    #[test]
    fn test_searcher_multiple_files() {
        let tmp_dir = TempDir::new().unwrap();

        std::fs::write(tmp_dir.path().join("a.rs"), "fn test_a() {}").unwrap();
        std::fs::write(tmp_dir.path().join("b.rs"), "fn test_b() {}").unwrap();
        std::fs::write(tmp_dir.path().join("c.rs"), "fn other() {}").unwrap();

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig::default();

        let matches = searcher
            .search("test_", &[tmp_dir.path()], &config)
            .unwrap();

        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn test_searcher_max_depth() {
        let tmp_dir = TempDir::new().unwrap();

        // Create nested structure
        let nested = tmp_dir.path().join("level1").join("level2");
        std::fs::create_dir_all(&nested).unwrap();

        std::fs::write(tmp_dir.path().join("root.rs"), "fn test() {}").unwrap();
        std::fs::write(tmp_dir.path().join("level1/mid.rs"), "fn test() {}").unwrap();
        std::fs::write(nested.join("deep.rs"), "fn test() {}").unwrap();

        let searcher = Searcher::new().unwrap();
        let config = SearchConfig {
            max_depth: Some(1),
            ..Default::default()
        };

        let matches = searcher.search("test", &[tmp_dir.path()], &config).unwrap();

        // Should only match root file with depth 1
        assert_eq!(matches.len(), 1);
    }

    // MatchSink tests
    #[test]
    fn test_match_sink_new() {
        let path = Path::new("test.rs");
        let sink = MatchSink::new(path);

        assert_eq!(sink.path, path);
        assert!(sink.matches.is_empty());
    }

    #[test]
    fn test_match_sink_into_matches() {
        let path = Path::new("test.rs");
        let sink = MatchSink::new(path);
        let matches = sink.into_matches();

        assert!(matches.is_empty());
    }

    // Error message constant test
    #[test]
    fn test_unsupported_mode_error_message() {
        assert!(UNSUPPORTED_TEXT_SEARCHER_MODE.contains("not supported by the text searcher"));
        assert!(UNSUPPORTED_TEXT_SEARCHER_MODE.contains("FallbackSearchEngine"));
        assert!(UNSUPPORTED_TEXT_SEARCHER_MODE.contains("CandidateGenerator"));
    }
}
