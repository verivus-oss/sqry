//! Query executor core - main `QueryExecutor` struct and orchestration
//!
//! # Query Execution Pipeline
//!
//! The executor processes queries via `CodeGraph` in three stages:
//!
//! 1. **Parse with cache** (`parse_query_ast`):
//!    - Check AST parse cache for `Arc<ParsedQuery>` (15-20ns cache hit)
//!    - On miss: parse query (1.6µs) and cache `Arc<ParsedQuery>`
//!    - Returns `Arc<ParsedQuery>` for zero-copy sharing
//!
//! 2. **Graph evaluation** (`execute_on_graph`):
//!    - Load `CodeGraph` from `.sqry/graph/snapshot.sqry`
//!    - Evaluate predicates directly on graph nodes
//!    - Handle relation queries (callers, callees, etc.) via graph edges
//!
//! 3. **Results**:
//!    - Return `QueryResults` with Arc-based accessors
//!    - Results sorted by file location
//!
//! # Arc-based Parse Cache
//!
//! The AST parse cache stores `Arc<ParsedQuery>` to enable zero-copy cache hits.
//! This provides:
//!
//! - **100× speedup** for cache hits (1.6µs → 16ns)
//! - **Thread-safe sharing** of parsed queries
//! - **Memory efficiency** (one `ParsedQuery` instance per unique query string)
//!
//! See [`AstParseCache`](crate::query::cache::AstParseCache) for benchmarks and details.

use super::graph_eval;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::persistence::load_from_path;
use crate::normalizer::MetadataNormalizer;
use crate::plugin::PluginManager;
use crate::query::cache::{CacheStats, ResultCache};
use crate::query::pipeline::AggregationResult;
use crate::query::plan::{CacheStatus, ExecutionStep, QueryPlan};
use crate::query::results::{JoinResults, QueryOutput, QueryResults};
use crate::query::types::{Expr, PipelineStage};
use anyhow::{Result, anyhow};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Thread-safe cache for a loaded code graph keyed by its canonical path.
///
/// The Option is None when no graph has been loaded yet. Once loaded, it stores
/// both the path the graph was loaded from and the graph itself.
pub(crate) type GraphCache = Arc<RwLock<Option<(PathBuf, Arc<CodeGraph>)>>>;

/// Executes queries against code using `CodeGraph`.
///
/// The executor loads a `CodeGraph` from disk and evaluates queries directly
/// against graph nodes using `execute_on_graph()`.
pub struct QueryExecutor {
    pub(crate) plugin_manager: PluginManager,

    /// Thread-safe cache for loaded `CodeGraph`
    ///
    /// See [`GraphCache`] type alias for details on the caching strategy.
    pub(crate) graph_cache: GraphCache,

    /// AST parse cache (query string → `ParsedQuery`) - Boolean parser
    pub(crate) ast_parse_cache: Arc<crate::query::cache::AstParseCache>,

    /// Result cache for query results
    pub(crate) result_cache: Arc<ResultCache>,

    /// Disable parallel query execution (for A/B performance testing)
    pub(crate) disable_parallel: bool,

    /// Validation options for query parsing
    pub(crate) validation_options: crate::query::validator::ValidationOptions,
}

impl QueryExecutor {
    /// Create a new query executor
    #[must_use]
    pub fn new() -> Self {
        Self {
            plugin_manager: PluginManager::new(),
            graph_cache: Arc::new(RwLock::new(None)),
            ast_parse_cache: Arc::new(crate::query::cache::AstParseCache::new(1000)),
            result_cache: Arc::new(ResultCache::new(1000)),
            disable_parallel: false,
            validation_options: crate::query::validator::ValidationOptions::default(),
        }
    }

    /// Create a query executor with a custom plugin manager
    #[must_use]
    pub fn with_plugin_manager(plugin_manager: PluginManager) -> Self {
        Self {
            plugin_manager,
            graph_cache: Arc::new(RwLock::new(None)),
            ast_parse_cache: Arc::new(crate::query::cache::AstParseCache::new(1000)),
            result_cache: Arc::new(ResultCache::new(1000)),
            disable_parallel: false,
            validation_options: crate::query::validator::ValidationOptions::default(),
        }
    }

    /// Return the plugin manager used by this executor.
    #[must_use]
    pub fn plugin_manager(&self) -> &PluginManager {
        &self.plugin_manager
    }

    fn build_registry(&self) -> crate::query::registry::FieldRegistry {
        let mut registry = crate::query::registry::FieldRegistry::with_core_fields();
        for plugin in self.plugin_manager.plugins() {
            let _collisions = registry.add_plugin_fields(plugin.fields());
        }

        let normalizer = MetadataNormalizer::new();
        for (short_form, canonical) in normalizer.mappings() {
            if registry.contains(canonical)
                && let Some(canonical_field) = registry.get(canonical)
            {
                let short_field = crate::query::types::FieldDescriptor {
                    name: short_form,
                    field_type: canonical_field.field_type.clone(),
                    operators: canonical_field.operators,
                    indexed: canonical_field.indexed,
                    doc: canonical_field.doc,
                };
                registry.add_field(short_field);
            }
        }

        registry
    }

    /// Configure validation options (e.g., fuzzy field tolerance)
    #[must_use]
    pub fn with_validation_options(
        mut self,
        options: crate::query::validator::ValidationOptions,
    ) -> Self {
        self.validation_options = options;
        self
    }

    /// Disable parallel query execution (for A/B performance testing)
    #[must_use]
    pub fn without_parallel(mut self) -> Self {
        self.disable_parallel = true;
        self
    }

    /// Get or load `CodeGraph` with thread-safe caching
    ///
    /// Uses double-checked locking pattern for thread-safe lazy initialization:
    /// 1. Try cache with read lock - fast path (validates path matches)
    /// 2. Load from disk with write lock - slow path with double-check
    ///
    /// # Path Tracking
    /// - Cache stores (`PathBuf`, `Arc<CodeGraph>`) to track which directory was loaded
    /// - If cached path != requested path, cache is invalidated and reloaded
    ///
    /// # Errors
    ///
    /// Returns an error if loading the graph from disk fails.
    pub(crate) fn get_or_load_graph(&self, dir: &Path) -> Result<Option<Arc<CodeGraph>>> {
        // Canonicalize the path for consistent comparisons
        let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());

        // Fast path: Try cache (read lock - allows concurrent reads)
        {
            let cache = self.graph_cache.read();
            if let Some((cached_path, graph)) = cache.as_ref()
                && cached_path == &canonical_dir
            {
                return Ok(Some(Arc::clone(graph)));
            }
            // Path mismatch - cache will be invalidated in slow path
        }

        // Slow path: Load and cache (write lock - exclusive access)
        let mut cache = self.graph_cache.write();

        // Double-check: another thread might have loaded while we waited for write lock
        if let Some((cached_path, graph)) = cache.as_ref() {
            if cached_path == &canonical_dir {
                return Ok(Some(Arc::clone(graph)));
            }
            // Path mismatch - invalidate cache and reload below
            log::debug!(
                "Graph cache invalidated due to path mismatch. Old: {}, New: {}",
                cached_path.display(),
                canonical_dir.display()
            );
        }

        // Actually load from disk
        let storage = crate::graph::unified::persistence::GraphStorage::new(&canonical_dir);

        if !storage.exists() {
            // No manifest → no complete index
            let auto_index_var = std::env::var("SQRY_AUTO_INDEX").unwrap_or_default();
            if auto_index_var == "false" || auto_index_var == "0" {
                *cache = None;
                return Ok(None);
            }

            log::info!(
                "No graph found at {}, auto-building index",
                canonical_dir.display()
            );
            // Release write lock before the heavy build operation
            drop(cache);

            let config = crate::graph::unified::build::BuildConfig::default();
            let (graph, _build_result) = crate::graph::unified::build::build_and_persist_graph(
                &canonical_dir,
                &self.plugin_manager,
                &config,
                "cli:auto_index",
            )?;
            let arc_graph = Arc::new(graph);

            let mut cache = self.graph_cache.write();
            *cache = Some((canonical_dir, Arc::clone(&arc_graph)));
            return Ok(Some(arc_graph));
        }

        // Manifest exists → try loading snapshot
        log::debug!(
            "Loading CodeGraph from: {}",
            storage.snapshot_path().display()
        );

        match load_from_path(storage.snapshot_path(), Some(&self.plugin_manager)) {
            Ok(graph) => {
                let arc_graph = Arc::new(graph);
                *cache = Some((canonical_dir, Arc::clone(&arc_graph)));
                Ok(Some(arc_graph))
            }
            Err(e) => {
                // Load failed (snapshot missing/corrupt) → auto-rebuild if enabled
                let auto_index_var = std::env::var("SQRY_AUTO_INDEX").unwrap_or_default();
                if auto_index_var == "false" || auto_index_var == "0" {
                    return Err(e.into());
                }
                log::warn!("Graph load failed ({e}), auto-rebuilding index");
                // Release write lock before the heavy rebuild
                drop(cache);

                let config = crate::graph::unified::build::BuildConfig::default();
                let (graph, _build_result) = crate::graph::unified::build::build_and_persist_graph(
                    &canonical_dir,
                    &self.plugin_manager,
                    &config,
                    "cli:auto_index",
                )?;
                let arc_graph = Arc::new(graph);

                let mut cache = self.graph_cache.write();
                *cache = Some((canonical_dir, Arc::clone(&arc_graph)));
                Ok(Some(arc_graph))
            }
        }
    }

    /// Get cache statistics (for monitoring/debugging)
    #[must_use]
    pub fn cache_stats(&self) -> (CacheStats, CacheStats) {
        (self.ast_parse_cache.stats(), self.result_cache.stats())
    }

    /// Get query execution plan for --explain
    ///
    /// Parses the query and returns detailed execution plan with timing,
    /// cache status, and optimization information.
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when query parsing, validation, or optimization fails.
    pub fn get_query_plan(&self, query_str: &str) -> Result<QueryPlan> {
        let start = Instant::now();

        // Step 1: Parse query (boolean AST)
        let parse_start = Instant::now();

        let parsed = self.parse_query_ast(query_str)?;
        let registry = self.build_registry();
        let optimizer = crate::query::optimizer::Optimizer::new(registry);
        let optimized_query = optimizer.optimize_query((*parsed.ast).clone());

        let optimized_query_str = format!("{:?}", optimized_query.root);
        let parse_step_name = "Parse query (boolean)";
        let steps_prefix = vec![
            (parse_step_name, 0),
            ("Validate fields", 0),
            ("Optimize AST", 0),
        ];

        // Duration beyond u64::MAX ms (~584 million years) is impossible; clamp to max
        let parse_time = parse_start
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);

        // Get cache status
        let (parse_stats, result_stats) = self.cache_stats();
        let cache_status = CacheStatus {
            parse_cache_hit: parse_stats.hits > 0,
            result_cache_hit: result_stats.hits > 0,
        };

        // Build execution steps
        let mut steps = Vec::new();
        let mut step_num = 1;

        for (operation, result_count) in steps_prefix {
            steps.push(ExecutionStep {
                step_num,
                operation: operation.to_string(),
                result_count,
                time_ms: if step_num == 1 { parse_time } else { 0 },
            });
            step_num += 1;
        }

        // Add graph lookup step
        steps.push(ExecutionStep {
            step_num,
            operation: "CodeGraph lookup".to_string(),
            result_count: 0,
            time_ms: 0,
        });

        // Duration beyond u64::MAX ms is impossible; clamp to max
        let total_time = start.elapsed().as_millis().try_into().unwrap_or(u64::MAX);

        Ok(QueryPlan::new(
            query_str.to_string(),
            optimized_query_str,
            steps,
            total_time,
            true, // Always uses CodeGraph
            cache_status,
        ))
    }

    /// Clear all caches (for testing)
    #[cfg(test)]
    pub fn clear_caches(&self) {
        self.ast_parse_cache.clear();
        self.result_cache.clear();
    }

    /// Parse query string using boolean AST parser with caching
    ///
    /// This method:
    /// 1. Checks AST parse cache for existing `ParsedQuery`
    /// 2. On cache miss: parses query, validates, extracts repo filter, normalizes
    /// 3. Caches the `ParsedQuery` for future reuse
    ///
    /// # Arguments
    ///
    /// * `query_str` - Query string to parse (boolean syntax)
    ///
    /// # Returns
    ///
    /// * `Ok(Arc<ParsedQuery>)` - Cached or freshly parsed query
    /// * `Err(...)` - Parse error, validation error, or repo filter error
    ///
    /// # Performance
    ///
    /// - Cache hit: ~15-20ns (Arc clone + hash lookup)
    /// - Cache miss: ~1.6µs (lex + parse + validate + normalize)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let executor = QueryExecutor::new();
    /// let parsed = executor.parse_query_ast("kind:function AND name:test")?;
    /// // parsed.ast contains the boolean expression
    /// // parsed.repo_filter contains any repo: predicates
    /// // parsed.normalized is the cache key (repo predicates stripped)
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when parsing, validation, or normalization fails.
    pub fn parse_query_ast(&self, query_str: &str) -> Result<Arc<crate::query::ParsedQuery>> {
        // Try cache first
        if let Some(cached_parsed) = self.ast_parse_cache.get(query_str) {
            log::trace!("AST parse cache HIT for: {query_str}");
            return Ok(cached_parsed);
        }

        log::trace!("AST parse cache MISS, parsing: {query_str}");

        // Parse query using boolean parser
        let ast = crate::query::parser_new::Parser::parse_query(query_str)
            .map_err(|err| err.with_source(query_str))?;

        let registry = self.build_registry();

        // Validate AST against registry (with normalization)
        let validator =
            crate::query::validator::Validator::with_options(registry, self.validation_options);
        let mut normalized_ast = ast.clone();
        normalized_ast.root = match validator.normalize_expr(&ast.root) {
            Ok(root) => root,
            Err(validation_err) => {
                // Wrap normalization error with source context for rich diagnostics
                let query_error = crate::query::error::QueryError::Validation(validation_err);
                return Err(query_error.with_source(query_str).into());
            }
        };
        if let Err(validation_err) = validator.validate(&normalized_ast.root) {
            // Wrap validation error with source context for rich diagnostics
            let query_error = crate::query::error::QueryError::Validation(validation_err);
            return Err(query_error.with_source(query_str).into());
        }

        // Create ParsedQuery (extracts repo filter, normalizes AST)
        let parsed = crate::query::ParsedQuery::from_ast(Arc::new(normalized_ast))?;

        // Cache the ParsedQuery for future reuse
        let arc_parsed = Arc::new(parsed);
        self.ast_parse_cache
            .insert_arc(query_str.to_string(), Arc::clone(&arc_parsed));

        Ok(arc_parsed)
    }
}

impl Default for QueryExecutor {
    fn default() -> Self {
        Self::new()
    }
}

impl QueryExecutor {
    /// Execute query using `CodeGraph`.
    ///
    /// Loads the `CodeGraph` from disk (or cache) and evaluates the query
    /// directly against graph nodes.
    ///
    /// # Arguments
    ///
    /// * `query` - The query string to parse and execute
    /// * `path` - Directory path containing the `.sqry/graph/snapshot.sqry` file
    ///
    /// # Returns
    ///
    /// `QueryResults` containing matched `NodeId`s with Arc-based accessors.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - No graph exists at the path (run `sqry index` first)
    /// - Query parsing fails
    /// - Predicate evaluation fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let executor = QueryExecutor::new();
    /// let results = executor.execute_on_graph("kind:function", Path::new("/my/project"))?;
    /// for m in results.iter() {
    ///     println!("{}: {}", m.kind().as_str(), m.name().unwrap_or_default());
    /// }
    /// ```
    pub fn execute_on_graph(&self, query: &str, path: &Path) -> Result<QueryResults> {
        self.execute_on_graph_with_variables(query, path, None)
    }

    /// Execute query with variable substitution.
    ///
    /// Variables in the query (e.g., `$type`) are replaced with values from
    /// the provided map before evaluation.
    ///
    /// # Errors
    ///
    /// Returns an error if graph loading, query parsing, variable resolution,
    /// or predicate evaluation fails.
    pub fn execute_on_graph_with_variables(
        &self,
        query: &str,
        path: &Path,
        variables: Option<&HashMap<String, String>>,
    ) -> Result<QueryResults> {
        let parsed = self.parse_query_ast(query)?;
        let graph = self
            .get_or_load_graph(path)?
            .ok_or_else(|| anyhow!("No graph found. Run `sqry index {}` first.", path.display()))?;

        let workspace_root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        self.execute_evaluate_with(graph, &parsed, &workspace_root, variables)
    }

    /// Execute a query against a caller-supplied `CodeGraph`, bypassing the
    /// graph cache and on-disk loading entirely.
    ///
    /// This is the daemon hot-path: the caller already holds the
    /// workspace-loaded `Arc<CodeGraph>` under a workspace lock, and we must
    /// NOT re-enter [`Self::get_or_load_graph`] (which would hit disk and
    /// pollute the executor's process-wide `graph_cache`). Query parsing still
    /// goes through the AST parse cache; only the graph acquisition path is
    /// skipped.
    ///
    /// The caller-supplied `workspace_root` is canonicalized by this method
    /// (mirroring [`Self::execute_on_graph_with_variables`]); the caller need
    /// not pre-canonicalize it. Passing the workspace root is the caller's
    /// responsibility — it is not derived from the graph.
    ///
    /// # Errors
    ///
    /// Returns an error if query parsing, variable resolution, or predicate
    /// evaluation fails. Unlike [`Self::execute_on_graph_with_variables`],
    /// this method cannot produce a "no graph found" error — the graph is
    /// always supplied by the caller.
    pub fn execute_on_preloaded_graph(
        &self,
        graph: Arc<CodeGraph>,
        query: &str,
        workspace_root: &Path,
        variables: Option<&HashMap<String, String>>,
    ) -> Result<QueryResults> {
        let parsed = self.parse_query_ast(query)?;
        let workspace_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());

        self.execute_evaluate_with(graph, &parsed, &workspace_root, variables)
    }

    /// Shared evaluation body for `execute_on_graph_with_variables` and
    /// `execute_on_preloaded_graph`.
    ///
    /// Both public entrypoints differ only in how they obtain the
    /// `Arc<CodeGraph>`; everything from variable resolution through
    /// `evaluate_all` + result assembly is identical and lives here to
    /// guarantee matching semantics.
    fn execute_evaluate_with(
        &self,
        graph: Arc<CodeGraph>,
        parsed: &crate::query::ParsedQuery,
        workspace_root: &Path,
        variables: Option<&HashMap<String, String>>,
    ) -> Result<QueryResults> {
        // Resolve variables if provided
        let effective_root = if let Some(vars) = variables {
            crate::query::types::resolve_variables(&parsed.ast.root, vars)
                .map_err(|e| anyhow!("Variable resolution error: {e}"))?
        } else {
            parsed.ast.root.clone()
        };

        let mut ctx = graph_eval::GraphEvalContext::new(&graph, &self.plugin_manager)
            .with_workspace_root(workspace_root)
            .with_parallel_disabled(self.disable_parallel);

        let matches = graph_eval::evaluate_all(&mut ctx, &effective_root)?;
        let mut results =
            QueryResults::new(graph, matches).with_workspace_root(workspace_root.to_path_buf());
        results.sort_by_location();
        Ok(results)
    }

    /// Execute a join query, returning matched node pairs.
    ///
    /// The query must have a `Join` expression at its root level.
    ///
    /// # Errors
    ///
    /// Returns an error if graph loading, query parsing, or join evaluation fails.
    pub fn execute_join(
        &self,
        query: &str,
        path: &Path,
        variables: Option<&HashMap<String, String>>,
    ) -> Result<JoinResults> {
        let parsed = self.parse_query_ast(query)?;
        let graph = self
            .get_or_load_graph(path)?
            .ok_or_else(|| anyhow!("No graph found. Run `sqry index {}` first.", path.display()))?;

        let workspace_root = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        // Resolve variables if provided
        let effective_root = if let Some(vars) = variables {
            crate::query::types::resolve_variables(&parsed.ast.root, vars)
                .map_err(|e| anyhow!("Variable resolution error: {e}"))?
        } else {
            parsed.ast.root.clone()
        };

        let Expr::Join(join) = &effective_root else {
            return Err(anyhow!(
                "Expected a join expression (e.g., `(kind:function) CALLS (kind:function)`)"
            ));
        };

        let ctx = graph_eval::GraphEvalContext::new(&graph, &self.plugin_manager)
            .with_workspace_root(&workspace_root)
            .with_parallel_disabled(self.disable_parallel);

        let eval_result = graph_eval::evaluate_join(&ctx, join, None)?;
        let results = JoinResults::new(
            graph,
            eval_result.pairs,
            join.edge.clone(),
            eval_result.truncated,
        )
        .with_workspace_root(workspace_root);
        Ok(results)
    }

    /// Execute a pipeline query (base query + aggregation stages).
    ///
    /// # Errors
    ///
    /// Returns an error if graph loading, query parsing, or pipeline execution fails.
    pub fn execute_pipeline(
        &self,
        query: &str,
        stages: &[PipelineStage],
        path: &Path,
        variables: Option<&HashMap<String, String>>,
    ) -> Result<Vec<AggregationResult>> {
        let results = self.execute_on_graph_with_variables(query, path, variables)?;

        let mut aggregations = Vec::new();
        for stage in stages {
            aggregations.push(super::pipeline::execute_pipeline_stage(&results, stage));
        }
        Ok(aggregations)
    }

    /// Execute a full query that may be a regular query, join, or pipeline.
    ///
    /// Detects the query type and dispatches to the appropriate executor.
    ///
    /// # Errors
    ///
    /// Returns an error if graph loading, query parsing, or execution fails.
    pub fn execute_full(
        &self,
        query: &str,
        path: &Path,
        variables: Option<&HashMap<String, String>>,
    ) -> Result<QueryOutput> {
        let parsed = self.parse_query_ast(query)?;

        // Check for join at the root level
        if matches!(&parsed.ast.root, Expr::Join(_)) {
            let join_results = self.execute_join(query, path, variables)?;
            return Ok(QueryOutput::Join(join_results));
        }

        // Check for pipeline
        if let Some(pipeline) = crate::query::parser_new::Parser::parse_pipeline_query(query)
            .map_err(|err| err.with_source(query))?
        {
            let aggregations = self.execute_pipeline(query, &pipeline.stages, path, variables)?;
            // Return the last aggregation result (chained stages reduce to final result)
            if let Some(last) = aggregations.into_iter().last() {
                return Ok(QueryOutput::Aggregation(last));
            }
        }

        // Regular query
        let results = self.execute_on_graph_with_variables(query, path, variables)?;
        Ok(QueryOutput::Results(results))
    }
}
