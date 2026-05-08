//! Fallback search engine combining semantic (AST) and text (ripgrep) search
//!
//! This module implements intelligent query execution that automatically falls back
//! to text search when semantic search returns insufficient results.
//!
//! Note: This is distinct from the embedding-based hybrid search,
//! which combines vectors + AST/graph. This module handles AST → ripgrep fallback.

use super::classifier::{QueryClassifier, QueryType};
use super::{Match as TextMatch, SearchConfig, SearchMode, Searcher as TextSearcher};
use crate::graph::CodeGraph;
use crate::query::QueryExecutor;
use crate::query::results::QueryResults;
use anyhow::{Context, Error, Result, anyhow};
use log::error;
use std::path::Path;
use std::sync::Arc;

/// Configuration for fallback search behavior
#[derive(Debug, Clone)]
pub struct FallbackConfig {
    /// Enable automatic fallback to text search (default: true)
    pub fallback_enabled: bool,

    /// Minimum semantic results before fallback (default: 1)
    /// If semantic search returns fewer than this, fallback to text
    pub min_semantic_results: usize,

    /// Context lines for text search results (default: 2)
    pub text_context_lines: usize,

    /// Maximum text search results (default: 1000)
    pub max_text_results: usize,

    /// Show which search mode was used (default: true)
    pub show_search_mode: bool,
}

impl Default for FallbackConfig {
    fn default() -> Self {
        Self {
            fallback_enabled: true,
            min_semantic_results: 1,
            text_context_lines: 2,
            max_text_results: 1000,
            show_search_mode: true,
        }
    }
}

impl FallbackConfig {
    /// Load configuration from environment variables
    ///
    /// Supported environment variables:
    /// - `SQRY_FALLBACK_ENABLED`: Enable/disable fallback (true/false)
    /// - `SQRY_MIN_SEMANTIC_RESULTS`: Minimum semantic results threshold
    /// - `SQRY_TEXT_CONTEXT_LINES`: Context lines for text results
    /// - `SQRY_MAX_TEXT_RESULTS`: Maximum text results
    /// - `SQRY_SHOW_SEARCH_MODE`: Show search mode (true/false)
    #[must_use]
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(val) = std::env::var("SQRY_FALLBACK_ENABLED") {
            config.fallback_enabled = val.parse().unwrap_or(true);
        }

        if let Ok(val) = std::env::var("SQRY_MIN_SEMANTIC_RESULTS") {
            config.min_semantic_results = val.parse().unwrap_or(1);
        }

        if let Ok(val) = std::env::var("SQRY_TEXT_CONTEXT_LINES") {
            config.text_context_lines = val.parse().unwrap_or(2);
        }

        if let Ok(val) = std::env::var("SQRY_MAX_TEXT_RESULTS") {
            config.max_text_results = val.parse().unwrap_or(1000);
        }

        if let Ok(val) = std::env::var("SQRY_SHOW_SEARCH_MODE") {
            config.show_search_mode = val.parse().unwrap_or(true);
        }

        config
    }
}

/// Search results from hybrid engine
#[derive(Debug)]
pub enum SearchResults {
    /// Semantic (CodeGraph-based) results
    Semantic {
        /// Results from semantic search
        results: QueryResults,
        /// Which search mode was used
        mode: SearchModeUsed,
    },

    /// Pure text (regex-based) results
    Text {
        /// Text matches found by ripgrep search
        matches: Vec<TextMatch>,
        /// Which search mode was used
        mode: SearchModeUsed,
    },
}

/// Which search mode was actually used
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchModeUsed {
    /// Semantic search only
    SemanticOnly,

    /// Text search only
    TextOnly,

    /// Semantic succeeded (no fallback needed)
    SemanticSucceeded,

    /// Semantic returned empty, fell back to text
    SemanticFallbackToText,

    /// Both semantic and text (combined)
    Combined,
}

impl SearchResults {
    /// Get the total number of results
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            SearchResults::Semantic { results, .. } => results.len(),
            SearchResults::Text { matches, .. } => matches.len(),
        }
    }

    /// Check if results are empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get the search mode that was used
    #[must_use]
    pub fn mode(&self) -> SearchModeUsed {
        match self {
            SearchResults::Semantic { mode, .. } | SearchResults::Text { mode, .. } => *mode,
        }
    }
}

/// Fallback search engine combining semantic and text search
pub struct FallbackSearchEngine {
    /// Semantic query executor (AST-based)
    query_executor: QueryExecutor,

    /// Text searcher (ripgrep-based)
    text_searcher: Option<TextSearcher>,

    /// Reason text searcher initialization failed (if unavailable)
    text_search_error: Option<String>,

    /// Configuration
    config: FallbackConfig,
}

impl FallbackSearchEngine {
    fn from_parts(
        query_executor: QueryExecutor,
        text_searcher: Option<TextSearcher>,
        text_search_error: Option<String>,
        config: FallbackConfig,
    ) -> Self {
        Self {
            query_executor,
            text_searcher,
            text_search_error,
            config,
        }
    }

    fn without_text_search(
        query_executor: QueryExecutor,
        config: FallbackConfig,
        error: &Error,
    ) -> Self {
        Self::from_parts(query_executor, None, Some(format!("{error:#}")), config)
    }

    fn text_searcher(&self) -> Result<&TextSearcher> {
        self.text_searcher.as_ref().ok_or_else(|| {
            let reason = self
                .text_search_error
                .as_deref()
                .unwrap_or("text searcher initialization failed");
            anyhow!("Text search is unavailable ({reason})")
        })
    }

    /// Create a new fallback search engine with default configuration
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] if the underlying [`FallbackSearchEngine::with_config`] call
    /// fails to construct a text searcher.
    pub fn new() -> Result<Self> {
        Self::with_config(FallbackConfig::default())
    }

    /// Create a fallback search engine with custom configuration
    ///
    /// **Note:** This creates a `QueryExecutor` without plugins, so metadata field queries
    /// like `async:true` and `visibility:public` will fail with "unknown field" errors.
    /// For metadata field support, use [`with_config_and_executor`](Self::with_config_and_executor)
    /// with a plugin-enabled executor.
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] if either the `QueryExecutor` or the text searcher cannot
    /// be initialised.
    pub fn with_config(config: FallbackConfig) -> Result<Self> {
        Self::with_config_and_executor(config, QueryExecutor::new())
    }

    /// Create fallback search engine with custom query executor (with plugins)
    ///
    /// This constructor allows passing a `QueryExecutor` that has plugin fields registered,
    /// enabling metadata queries like `async:true` and `visibility:public` in hybrid mode.
    ///
    /// # Arguments
    /// * `config` - Hybrid search configuration
    /// * `query_executor` - Pre-configured `QueryExecutor` with plugin fields
    ///
    /// # Example
    /// ```no_run
    /// use sqry_core::search::fallback::{FallbackSearchEngine, FallbackConfig};
    /// use sqry_core::query::QueryExecutor;
    /// use sqry_core::plugin::PluginManager;
    ///
    /// // Create plugin manager and register built-in plugins
    /// let mut plugin_manager = PluginManager::new();
    /// // Register plugins as needed (e.g., in CLI):
    /// // plugin_manager.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    /// // plugin_manager.register_builtin(Box::new(sqry_lang_python::PythonPlugin::default()));
    /// // ... register other plugins
    ///
    /// let query_executor = QueryExecutor::with_plugin_manager(plugin_manager);
    /// let config = FallbackConfig::default();
    /// let engine = FallbackSearchEngine::with_config_and_executor(config, query_executor);
    /// ```
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the ripgrep-based text searcher fails to initialise.
    pub fn with_config_and_executor(
        config: FallbackConfig,
        query_executor: QueryExecutor,
    ) -> Result<Self> {
        let text_searcher =
            TextSearcher::new().context("Failed to create text searcher for hybrid engine")?;

        Ok(Self::from_parts(
            query_executor,
            Some(text_searcher),
            None,
            config,
        ))
    }

    /// Search with automatic mode detection and fallback
    ///
    /// # Arguments
    /// * `query` - The search query
    /// * `path` - The path to search in
    ///
    /// # Returns
    /// `SearchResults` with the mode used and matched symbols/text
    ///
    /// # Example
    /// ```no_run
    /// use sqry_core::search::fallback::FallbackSearchEngine;
    /// use std::path::Path;
    ///
    /// let mut engine = FallbackSearchEngine::new().unwrap();
    /// // Search for functions in current directory
    /// let results = engine.search("kind:function", Path::new("."));
    /// ```
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] if either semantic or text search fails and no fallback
    /// mode can recover (for example, when both engines encounter I/O errors).
    pub fn search(&mut self, query: &str, path: &Path) -> Result<SearchResults> {
        // Step 1: Classify query
        let query_type = QueryClassifier::classify(query);

        if self.config.show_search_mode {
            match query_type {
                QueryType::Semantic => log::debug!("[Semantic search mode]"),
                QueryType::Text => log::debug!("[Text search mode]"),
                QueryType::Hybrid => log::debug!("[Hybrid mode: trying semantic first...]"),
            }
        }

        // Step 2: Execute based on classification
        match query_type {
            QueryType::Semantic => self.search_semantic_only(query, path),
            QueryType::Text => self.search_text_only(query, path),
            QueryType::Hybrid => self.search_hybrid(query, path),
        }
    }

    /// Force semantic search only (no fallback)
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when the semantic query executor fails to parse or execute
    /// the request (invalid syntax, missing graph, or predicate errors).
    pub fn search_semantic_only(&mut self, query: &str, path: &Path) -> Result<SearchResults> {
        // Execute query using CodeGraph
        let results = self.query_executor.execute_on_graph(query, path)?;

        Ok(SearchResults::Semantic {
            results,
            mode: SearchModeUsed::SemanticOnly,
        })
    }

    /// SGA03 Major #1 — semantic-only search against a caller-supplied
    /// [`CodeGraph`].
    ///
    /// Identical contract to [`Self::search_semantic_only`] but routes the
    /// semantic execution through
    /// [`QueryExecutor::execute_on_preloaded_graph`] instead of
    /// [`QueryExecutor::execute_on_graph`]. This is the entrypoint the CLI
    /// hybrid path uses after the shared `FilesystemGraphProvider` has
    /// already acquired the workspace graph: re-loading from disk inside
    /// the executor would bypass the provider's plugin/manifest checks.
    ///
    /// `scope_path` is the **search scope** (a directory or file under the
    /// workspace) — not the workspace root. The executor canonicalises it
    /// internally before evaluating file-scope predicates, mirroring
    /// [`QueryExecutor::execute_on_graph_with_variables`].
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when query parsing, variable resolution, or
    /// predicate evaluation fails. Cannot produce a "no graph found" error —
    /// the graph is always supplied by the caller.
    pub fn search_semantic_only_with_preloaded_graph(
        &mut self,
        query: &str,
        graph: Arc<CodeGraph>,
        scope_path: &Path,
    ) -> Result<SearchResults> {
        let results = self
            .query_executor
            .execute_on_preloaded_graph(graph, query, scope_path, None)?;

        Ok(SearchResults::Semantic {
            results,
            mode: SearchModeUsed::SemanticOnly,
        })
    }

    /// Force text search only (no semantic attempt)
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when text search is disabled/unavailable or when ripgrep
    /// returns an error while scanning the requested paths.
    pub fn search_text_only(&mut self, query: &str, path: &Path) -> Result<SearchResults> {
        let config = SearchConfig {
            mode: SearchMode::Regex,
            case_insensitive: false,
            include_hidden: false,
            follow_symlinks: false,
            max_depth: None,
            file_types: Vec::new(),
            exclude_patterns: Vec::new(),
            before_context: self.config.text_context_lines,
            after_context: self.config.text_context_lines,
        };

        let searcher = self
            .text_searcher()
            .context("Text search unavailable in hybrid engine")?;

        let matches = searcher
            .search(query, &[path], &config)
            .context("Text search failed")?;

        // Limit results
        let matches = matches
            .into_iter()
            .take(self.config.max_text_results)
            .collect();

        Ok(SearchResults::Text {
            matches,
            mode: SearchModeUsed::TextOnly,
        })
    }

    /// SGA03 Major #1 — hybrid auto-classified search against a
    /// caller-supplied [`CodeGraph`].
    ///
    /// Mirrors [`Self::search`] but threads the provider-acquired graph
    /// into every semantic execution. The text-only branch does not need
    /// the graph and matches [`Self::search_text_only`] verbatim.
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] if either semantic or text search fails
    /// and no fallback mode can recover.
    pub fn search_with_preloaded_graph(
        &mut self,
        query: &str,
        graph: Arc<CodeGraph>,
        scope_path: &Path,
    ) -> Result<SearchResults> {
        let query_type = QueryClassifier::classify(query);

        if self.config.show_search_mode {
            match query_type {
                QueryType::Semantic => log::debug!("[Semantic search mode]"),
                QueryType::Text => log::debug!("[Text search mode]"),
                QueryType::Hybrid => log::debug!("[Hybrid mode: trying semantic first...]"),
            }
        }

        match query_type {
            QueryType::Semantic => {
                self.search_semantic_only_with_preloaded_graph(query, graph, scope_path)
            }
            QueryType::Text => self.search_text_only(query, scope_path),
            QueryType::Hybrid => self.search_hybrid_with_preloaded_graph(query, graph, scope_path),
        }
    }

    /// Hybrid search: try semantic first, fallback to text if needed
    fn search_hybrid(&mut self, query: &str, path: &Path) -> Result<SearchResults> {
        // Try semantic search first using CodeGraph
        let semantic_result = self.query_executor.execute_on_graph(query, path);

        match semantic_result {
            Ok(results) if results.len() >= self.config.min_semantic_results => {
                // Semantic search succeeded with sufficient results
                if self.config.show_search_mode {
                    log::debug!("[Semantic search: {} results]", results.len());
                }

                Ok(SearchResults::Semantic {
                    results,
                    mode: SearchModeUsed::SemanticSucceeded,
                })
            }

            Ok(results) if self.config.fallback_enabled => {
                // Semantic returned too few results - fallback to text
                if self.config.show_search_mode {
                    log::debug!(
                        "[Semantic search: {} results (below threshold {})]",
                        results.len(),
                        self.config.min_semantic_results
                    );
                    log::debug!("[Falling back to text search...]");
                }

                let config = SearchConfig {
                    mode: SearchMode::Regex,
                    case_insensitive: false,
                    include_hidden: false,
                    follow_symlinks: false,
                    max_depth: None,
                    file_types: Vec::new(),
                    exclude_patterns: Vec::new(),
                    before_context: self.config.text_context_lines,
                    after_context: self.config.text_context_lines,
                };

                let searcher = self
                    .text_searcher()
                    .context("Text search unavailable during fallback")?;
                let matches = searcher
                    .search(query, &[path], &config)
                    .context("Text search failed during fallback")?;

                let matches = matches
                    .into_iter()
                    .take(self.config.max_text_results)
                    .collect::<Vec<_>>();

                if self.config.show_search_mode {
                    log::debug!("[Text search: {} results]", matches.len());
                }

                Ok(SearchResults::Text {
                    matches,
                    mode: SearchModeUsed::SemanticFallbackToText,
                })
            }

            Ok(results) => {
                // Fallback disabled, return semantic results as-is
                Ok(SearchResults::Semantic {
                    results,
                    mode: SearchModeUsed::SemanticOnly,
                })
            }

            Err(e) if self.config.fallback_enabled => {
                // Semantic search failed - fallback to text
                if self.config.show_search_mode {
                    log::debug!("[Semantic search failed: {e}]");
                    log::debug!("[Falling back to text search...]");
                }

                let config = SearchConfig {
                    mode: SearchMode::Regex,
                    case_insensitive: false,
                    include_hidden: false,
                    follow_symlinks: false,
                    max_depth: None,
                    file_types: Vec::new(),
                    exclude_patterns: Vec::new(),
                    before_context: self.config.text_context_lines,
                    after_context: self.config.text_context_lines,
                };

                let searcher = self
                    .text_searcher()
                    .context("Text search unavailable during fallback")?;
                let matches = searcher
                    .search(query, &[path], &config)
                    .context("Text search failed during fallback")?;

                let matches = matches
                    .into_iter()
                    .take(self.config.max_text_results)
                    .collect();

                Ok(SearchResults::Text {
                    matches,
                    mode: SearchModeUsed::SemanticFallbackToText,
                })
            }

            Err(e) => {
                // Fallback disabled and semantic failed - return error
                Err(e)
            }
        }
    }

    /// SGA03 Major #1 — hybrid search against a caller-supplied
    /// [`CodeGraph`].
    ///
    /// Functionally identical to [`Self::search_hybrid`] except the
    /// semantic attempt runs through
    /// [`QueryExecutor::execute_on_preloaded_graph`] so the
    /// provider-acquired graph is the single source of truth — the
    /// executor's process-wide cache is **not** consulted and
    /// [`crate::graph::unified::persistence::load_from_path`] is **not**
    /// re-entered. The text-fallback branches reuse the existing
    /// [`super::Searcher`] verbatim.
    fn search_hybrid_with_preloaded_graph(
        &mut self,
        query: &str,
        graph: Arc<CodeGraph>,
        path: &Path,
    ) -> Result<SearchResults> {
        let semantic_result = self
            .query_executor
            .execute_on_preloaded_graph(graph, query, path, None);

        match semantic_result {
            Ok(results) if results.len() >= self.config.min_semantic_results => {
                if self.config.show_search_mode {
                    log::debug!("[Semantic search: {} results]", results.len());
                }

                Ok(SearchResults::Semantic {
                    results,
                    mode: SearchModeUsed::SemanticSucceeded,
                })
            }

            Ok(results) if self.config.fallback_enabled => {
                if self.config.show_search_mode {
                    log::debug!(
                        "[Semantic search: {} results (below threshold {})]",
                        results.len(),
                        self.config.min_semantic_results
                    );
                    log::debug!("[Falling back to text search...]");
                }

                self.text_fallback(query, path, SearchModeUsed::SemanticFallbackToText)
            }

            Ok(results) => Ok(SearchResults::Semantic {
                results,
                mode: SearchModeUsed::SemanticOnly,
            }),

            Err(e) if self.config.fallback_enabled => {
                if self.config.show_search_mode {
                    log::debug!("[Semantic search failed: {e}]");
                    log::debug!("[Falling back to text search...]");
                }

                self.text_fallback(query, path, SearchModeUsed::SemanticFallbackToText)
            }

            Err(e) => Err(e),
        }
    }

    /// Shared text-fallback path used by hybrid variants. Mirrors the
    /// inline blocks in [`Self::search_hybrid`] so the preloaded-graph
    /// hybrid path produces identical text fallbacks.
    fn text_fallback(
        &self,
        query: &str,
        path: &Path,
        mode: SearchModeUsed,
    ) -> Result<SearchResults> {
        let config = SearchConfig {
            mode: SearchMode::Regex,
            case_insensitive: false,
            include_hidden: false,
            follow_symlinks: false,
            max_depth: None,
            file_types: Vec::new(),
            exclude_patterns: Vec::new(),
            before_context: self.config.text_context_lines,
            after_context: self.config.text_context_lines,
        };

        let searcher = self
            .text_searcher()
            .context("Text search unavailable during fallback")?;
        let matches = searcher
            .search(query, &[path], &config)
            .context("Text search failed during fallback")?;

        let matches = matches
            .into_iter()
            .take(self.config.max_text_results)
            .collect::<Vec<_>>();

        if self.config.show_search_mode {
            log::debug!("[Text search: {} results]", matches.len());
        }

        Ok(SearchResults::Text { matches, mode })
    }
}

impl Default for FallbackSearchEngine {
    fn default() -> Self {
        let config = FallbackConfig::default();
        let query_executor = QueryExecutor::new();
        match TextSearcher::new() {
            Ok(searcher) => Self::from_parts(query_executor, Some(searcher), None, config),
            Err(err) => {
                error!(
                    "FallbackSearchEngine default initialization failed; text search disabled: {err:#}"
                );
                Self::without_text_search(query_executor, config, &err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn setup_test_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let test_file = dir.path().join("test.rs");
        fs::write(
            &test_file,
            r#"
pub fn foo() {
    // TODO: add more functionality
    println!("hello");
}

// bar intentionally simplified for fixture use
fn bar() {
    // Stubbed for test fixture coverage only
}
"#,
        )
        .unwrap();

        dir
    }

    #[test]
    fn test_hybrid_config_default() {
        let config = FallbackConfig::default();
        assert!(config.fallback_enabled);
        assert_eq!(config.min_semantic_results, 1);
        assert_eq!(config.text_context_lines, 2);
        assert_eq!(config.max_text_results, 1000);
    }

    #[test]
    fn test_hybrid_config_from_env() {
        unsafe {
            std::env::set_var("SQRY_FALLBACK_ENABLED", "false");
            std::env::set_var("SQRY_MIN_SEMANTIC_RESULTS", "5");
            std::env::set_var("SQRY_TEXT_CONTEXT_LINES", "3");
        }

        let config = FallbackConfig::from_env();
        assert!(!config.fallback_enabled);
        assert_eq!(config.min_semantic_results, 5);
        assert_eq!(config.text_context_lines, 3);

        // Cleanup
        unsafe {
            std::env::remove_var("SQRY_FALLBACK_ENABLED");
            std::env::remove_var("SQRY_MIN_SEMANTIC_RESULTS");
            std::env::remove_var("SQRY_TEXT_CONTEXT_LINES");
        }
    }

    #[test]
    fn test_search_text_only() {
        let dir = setup_test_dir();
        let mut engine = FallbackSearchEngine::new().unwrap();

        let results = engine.search_text_only("TODO", dir.path()).unwrap();

        match results {
            SearchResults::Text { matches, mode } => {
                assert!(!matches.is_empty());
                assert_eq!(mode, SearchModeUsed::TextOnly);
            }
            SearchResults::Semantic { .. } => panic!("Expected Text results"),
        }
    }

    #[test]
    fn test_search_results_len() {
        let dir = setup_test_dir();
        let mut engine = FallbackSearchEngine::new().unwrap();

        let results = engine.search_text_only("TODO", dir.path()).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_hybrid_fallback_disabled() {
        let dir = setup_test_dir();
        let config = FallbackConfig {
            fallback_enabled: false,
            ..Default::default()
        };

        let mut engine = FallbackSearchEngine::with_config(config).unwrap();

        // This should fail with fallback disabled (no semantic match for "TODO")
        let result = engine.search_hybrid("nonexistent", dir.path());

        // Should return empty semantic results (not fallback to text)
        if let Ok(SearchResults::Semantic { results, .. }) = result {
            assert_eq!(results.len(), 0);
        }
    }

    /// SGA03 Major #1 (codex iter2) — preloaded-graph entrypoints route
    /// through `QueryExecutor::execute_on_preloaded_graph`, never through
    /// the executor's `execute_on_graph` cache+disk-load path.
    ///
    /// The proof here is *negative*: pass an empty in-memory `CodeGraph`
    /// against a `path` whose ancestors contain no `.sqry/graph` artifact.
    /// If the engine were still using `execute_on_graph`, the executor's
    /// `get_or_load_graph` would fail with "No graph found. Run `sqry
    /// index ...`". Because the entrypoint takes the caller's graph
    /// directly, the call must succeed and return zero semantic matches.
    #[test]
    fn semantic_only_with_preloaded_graph_uses_caller_graph() {
        let dir = TempDir::new().unwrap();
        // Deliberately *no* `.sqry/graph/...` artifact under `dir`.

        let mut engine = FallbackSearchEngine::new().unwrap();
        let graph = Arc::new(CodeGraph::new());

        let results = engine
            .search_semantic_only_with_preloaded_graph("kind:function", graph, dir.path())
            .expect("preloaded-graph entrypoint must not consult on-disk graph");

        match results {
            SearchResults::Semantic { results, mode } => {
                assert_eq!(results.len(), 0, "empty graph yields zero matches");
                assert_eq!(mode, SearchModeUsed::SemanticOnly);
            }
            SearchResults::Text { .. } => {
                panic!("semantic-only entrypoint must not return text results")
            }
        }
    }

    /// Companion to the above for the hybrid auto-classify entrypoint:
    /// against an empty preloaded graph + no on-disk artifact, the
    /// semantic attempt must come back with zero results and (because
    /// `kind:function` is a Semantic-classified query) the engine must
    /// not silently downgrade to text fallback.
    #[test]
    fn search_with_preloaded_graph_routes_semantic_class_to_preloaded_executor() {
        let dir = TempDir::new().unwrap();
        let mut engine = FallbackSearchEngine::new().unwrap();
        let graph = Arc::new(CodeGraph::new());

        let results = engine
            .search_with_preloaded_graph("kind:function", graph, dir.path())
            .expect("preloaded-graph entrypoint must not consult on-disk graph");

        match results {
            SearchResults::Semantic { results, mode } => {
                assert_eq!(results.len(), 0);
                assert_eq!(mode, SearchModeUsed::SemanticOnly);
            }
            SearchResults::Text { .. } => panic!("semantic-class query must not fall back to text"),
        }
    }
}
