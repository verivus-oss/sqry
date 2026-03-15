//! `WorkspaceIndex`: multi-repo search orchestration using `SessionManager`.

use std::path::{Path, PathBuf};

use crate::graph::unified::NodeKind;
use crate::query::QueryExecutor;
use crate::session::SessionManager;

use super::error::{WorkspaceError, WorkspaceResult};
use super::registry::{WorkspaceRegistry, WorkspaceRepository};

/// A workspace-level index that orchestrates queries across multiple repositories.
///
/// `WorkspaceIndex` delegates to `SessionManager` for individual repository queries,
/// leveraging its caching and lazy-loading capabilities. Results are aggregated
/// and tagged with repository metadata.
pub struct WorkspaceIndex {
    registry: WorkspaceRegistry,
    session: SessionManager,
    workspace_root: PathBuf,
}

impl WorkspaceIndex {
    /// Open a workspace index from the registry file at the given path.
    ///
    /// # Arguments
    ///
    /// * `workspace_root` - Root directory of the workspace
    /// * `registry_path` - Path to the .sqry-workspace registry file
    ///
    /// # Example
    ///
    /// ```no_run
    /// use sqry_core::workspace::WorkspaceIndex;
    /// use std::path::Path;
    ///
    /// let workspace = Path::new("/path/to/workspace");
    /// let registry = workspace.join(".sqry-workspace");
    /// let mut index = WorkspaceIndex::open(workspace, &registry).unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when registry loading or session creation fails.
    pub fn open(workspace_root: impl Into<PathBuf>, registry_path: &Path) -> WorkspaceResult<Self> {
        let workspace_root = workspace_root.into();
        let registry = WorkspaceRegistry::load(registry_path)?;
        let session = SessionManager::new().map_err(|e| WorkspaceError::QueryParsing {
            message: format!("Failed to create SessionManager: {e}"),
        })?;

        Ok(Self {
            registry,
            session,
            workspace_root,
        })
    }

    /// Create a new workspace index with an existing registry and session.
    ///
    /// Useful for testing or when you want to provide a custom `SessionManager`.
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        registry: WorkspaceRegistry,
        session: SessionManager,
    ) -> Self {
        Self {
            registry,
            session,
            workspace_root: workspace_root.into(),
        }
    }

    /// Execute a query across all repositories in the workspace.
    ///
    /// This method:
    /// 1. Parses the query into AST and extracts repo filter
    /// 2. Filters repositories based on the repo predicates
    /// 3. Executes the normalized query (repo predicates stripped) against each matching repository
    /// 4. Aggregates results with repository metadata
    ///
    /// # Arguments
    ///
    /// * `query` - The query string to execute (may include repo: predicates)
    ///
    /// # Returns
    ///
    /// A list of matches with their associated repository information.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use sqry_core::workspace::WorkspaceIndex;
    /// use std::path::Path;
    ///
    /// let workspace = Path::new("/path/to/workspace");
    /// let registry = workspace.join(".sqry-workspace");
    /// let mut index = WorkspaceIndex::open(workspace, &registry).unwrap();
    ///
    /// let results = index.query("kind:function AND repo:backend-*").unwrap();
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when parsing the query or executing repository queries fails.
    pub fn query(&mut self, query: &str) -> WorkspaceResult<Vec<NodeWithRepo>> {
        // Parse query into AST to extract repo filter.
        let executor = QueryExecutor::new();
        let parsed = executor
            .parse_query_ast(query)
            .map_err(|e| WorkspaceError::QueryParsing {
                message: format!("Failed to parse query: {e}"),
            })?;

        // Filter repositories based on the repo predicates
        let repos: Vec<&WorkspaceRepository> = self
            .registry
            .repositories
            .iter()
            .filter(|repo| parsed.repo_filter.matches(&repo.name))
            .collect();

        if repos.is_empty() {
            return Ok(Vec::new());
        }

        // Use normalized query string (repo predicates stripped) for repository execution
        // The repo filtering has already been done at the workspace level above
        let query_str = parsed.normalized.as_ref();

        // Execute query against each repository and aggregate results
        let mut all_results = Vec::new();

        for repo in repos {
            // Resolve absolute path for the repository
            let repo_path = if repo.root.is_absolute() {
                repo.root.clone()
            } else {
                self.workspace_root.join(&repo.root)
            };

            // Query the repository via SessionManager
            // SessionManager will parse the query again and execute it against the single-repo index
            match self.session.query(&repo_path, query_str) {
                Ok(results) => {
                    // Tag each result with repo metadata
                    for m in results.iter() {
                        let match_info = MatchInfo {
                            name: m.name().map(|s| s.to_string()).unwrap_or_default(),
                            qualified_name: m.qualified_name().map(|s| s.to_string()),
                            kind: m.kind(),
                            language: m.language().map(|lang| lang.to_string()),
                            file_path: m.relative_path().unwrap_or_default(),
                            start_line: m.start_line(),
                            start_column: m.start_column(),
                            end_line: m.end_line(),
                            end_column: m.end_column(),
                            is_static: m.is_static(),
                            signature: m.signature().map(|s| s.to_string()),
                            doc: m.doc().map(|s| s.to_string()),
                        };
                        all_results.push(NodeWithRepo {
                            match_info,
                            repo_name: repo.name.clone(),
                            repo_id: repo.id.clone(),
                            repo_path: repo_path.clone(),
                        });
                    }
                }
                Err(err) => {
                    // Log error but continue with other repos
                    eprintln!(
                        "Warning: Failed to query repository '{}': {}",
                        repo.name, err
                    );
                }
            }
        }

        Ok(all_results)
    }

    /// Get workspace-level statistics.
    ///
    /// Returns aggregated stats across all repositories in the workspace.
    #[must_use]
    pub fn stats(&self) -> WorkspaceStats {
        let total_repos = self.registry.repositories.len();
        let indexed_repos = self
            .registry
            .repositories
            .iter()
            .filter(|r| r.last_indexed_at.is_some())
            .count();
        let total_symbols = self
            .registry
            .repositories
            .iter()
            .filter_map(|r| r.symbol_count)
            .sum();

        WorkspaceStats {
            total_repos,
            indexed_repos,
            total_symbols,
        }
    }

    /// Get detailed workspace statistics including staleness tracking.
    ///
    /// Returns comprehensive stats with freshness buckets, health scores,
    /// and other detailed metrics.
    #[must_use]
    pub fn detailed_stats(&self) -> super::stats::DetailedWorkspaceStats {
        super::stats::DetailedWorkspaceStats::from_registry(&self.registry)
    }

    /// Get a reference to the underlying registry.
    #[must_use]
    pub fn registry(&self) -> &WorkspaceRegistry {
        &self.registry
    }

    /// Get a mutable reference to the underlying registry.
    pub fn registry_mut(&mut self) -> &mut WorkspaceRegistry {
        &mut self.registry
    }

    /// Get the workspace root directory.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }
}

/// Query match information from `CodeGraph`.
#[derive(Debug, Clone)]
pub struct MatchInfo {
    /// Node name
    pub name: String,
    /// Canonical qualified name, if available.
    pub qualified_name: Option<String>,
    /// Node kind (function, class, etc.)
    pub kind: NodeKind,
    /// Language derived from the backing file, if available.
    pub language: Option<String>,
    /// File path where the node is defined (relative to repo root)
    pub file_path: PathBuf,
    /// Starting line number (1-based)
    pub start_line: u32,
    /// Starting column (1-based)
    pub start_column: u32,
    /// Ending line number (1-based)
    pub end_line: u32,
    /// Ending column (1-based)
    pub end_column: u32,
    /// Whether this match represents a static member.
    pub is_static: bool,
    /// Optional signature
    pub signature: Option<String>,
    /// Optional documentation
    pub doc: Option<String>,
}

/// A query match tagged with repository metadata for workspace-level queries.
#[derive(Debug, Clone)]
pub struct NodeWithRepo {
    /// The match info from the query.
    pub match_info: MatchInfo,
    /// The name of the repository containing this node.
    pub repo_name: String,
    /// The unique identifier of the repository containing this node.
    pub repo_id: super::registry::WorkspaceRepoId,
    /// Absolute path to the repository containing this node.
    pub repo_path: PathBuf,
}

/// Aggregated statistics for a workspace.
#[derive(Debug, Clone)]
pub struct WorkspaceStats {
    /// Total number of repositories in the workspace.
    pub total_repos: usize,
    /// Number of repositories that have been indexed.
    pub indexed_repos: usize,
    /// Total symbol count across all indexed repositories.
    pub total_symbols: u64,
}
