//! Workspace statistics and staleness tracking.

use std::time::{Duration, SystemTime};

use super::registry::{WorkspaceRegistry, WorkspaceRepository};

fn u64_to_f64(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

fn usize_to_f64(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    {
        value as f64
    }
}

/// Detailed workspace statistics including staleness tracking.
#[derive(Debug, Clone)]
pub struct DetailedWorkspaceStats {
    /// Total number of repositories in the workspace.
    pub total_repos: usize,
    /// Number of repositories that have been indexed.
    pub indexed_repos: usize,
    /// Number of repositories that have never been indexed.
    pub unindexed_repos: usize,
    /// Total symbol count across all indexed repositories.
    pub total_symbols: u64,
    /// Freshness buckets categorizing repositories by age.
    pub freshness: FreshnessBuckets,
    /// Average symbols per indexed repository.
    pub avg_symbols_per_repo: f64,
}

/// Freshness buckets categorizing repositories by last indexed time.
#[derive(Debug, Clone, Default)]
pub struct FreshnessBuckets {
    /// Repositories indexed within the last hour.
    pub fresh: usize,
    /// Repositories indexed within the last 24 hours.
    pub recent: usize,
    /// Repositories indexed within the last 7 days.
    pub stale: usize,
    /// Repositories indexed more than 7 days ago.
    pub very_stale: usize,
    /// Repositories that have never been indexed.
    pub never_indexed: usize,
}

impl FreshnessBuckets {
    /// Calculate freshness buckets from a registry.
    #[must_use]
    pub fn from_registry(registry: &WorkspaceRegistry) -> Self {
        let now = SystemTime::now();
        let mut buckets = Self::default();

        for repo in &registry.repositories {
            if let Some(last_indexed) = repo.last_indexed_at {
                if let Ok(elapsed) = now.duration_since(last_indexed) {
                    buckets.categorize(elapsed);
                } else {
                    // Future timestamp (clock skew) - treat as fresh
                    buckets.fresh += 1;
                }
            } else {
                buckets.never_indexed += 1;
            }
        }

        buckets
    }

    fn categorize(&mut self, elapsed: Duration) {
        const HOUR: Duration = Duration::from_secs(3600);
        const DAY: Duration = Duration::from_secs(86400);
        const WEEK: Duration = Duration::from_secs(604_800);

        if elapsed < HOUR {
            self.fresh += 1;
        } else if elapsed < DAY {
            self.recent += 1;
        } else if elapsed < WEEK {
            self.stale += 1;
        } else {
            self.very_stale += 1;
        }
    }

    /// Get the total number of indexed repositories across all buckets.
    #[must_use]
    pub fn indexed_total(&self) -> usize {
        self.fresh + self.recent + self.stale + self.very_stale
    }

    /// Get the total number of repositories (including never indexed).
    #[must_use]
    pub fn total(&self) -> usize {
        self.indexed_total() + self.never_indexed
    }
}

impl DetailedWorkspaceStats {
    /// Compute detailed statistics from a workspace registry.
    #[must_use]
    pub fn from_registry(registry: &WorkspaceRegistry) -> Self {
        let total_repos = registry.repositories.len();
        let indexed_repos = registry
            .repositories
            .iter()
            .filter(|r| r.last_indexed_at.is_some())
            .count();
        let unindexed_repos = total_repos - indexed_repos;

        let total_symbols: u64 = registry
            .repositories
            .iter()
            .filter_map(|r| r.symbol_count)
            .sum();

        let avg_symbols_per_repo = if indexed_repos > 0 {
            u64_to_f64(total_symbols) / usize_to_f64(indexed_repos)
        } else {
            0.0
        };

        let freshness = FreshnessBuckets::from_registry(registry);

        Self {
            total_repos,
            indexed_repos,
            unindexed_repos,
            total_symbols,
            freshness,
            avg_symbols_per_repo,
        }
    }

    /// Get repositories that need reindexing (older than threshold).
    #[must_use]
    pub fn stale_repos<'a>(
        &self,
        registry: &'a WorkspaceRegistry,
        threshold: Duration,
    ) -> Vec<&'a WorkspaceRepository> {
        let now = SystemTime::now();
        registry
            .repositories
            .iter()
            .filter(|repo| {
                if let Some(last_indexed) = repo.last_indexed_at
                    && let Ok(elapsed) = now.duration_since(last_indexed)
                {
                    return elapsed > threshold;
                }
                // Never indexed or future timestamp
                repo.last_indexed_at.is_none()
            })
            .collect()
    }

    /// Calculate a health score (0.0-1.0) based on freshness.
    ///
    /// Score factors:
    /// - Fresh repos (< 1 hour): 1.0 weight
    /// - Recent repos (< 1 day): 0.8 weight
    /// - Stale repos (< 1 week): 0.5 weight
    /// - Very stale repos (> 1 week): 0.2 weight
    /// - Never indexed: 0.0 weight
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        reason = "Freshness scoring is informational; f64 is adequate for UX metrics"
    )]
    pub fn health_score(&self) -> f64 {
        if self.total_repos == 0 {
            return 1.0;
        }

        // Casts to f64 are lossy for very large counts; acceptable for display-level scoring.
        #[allow(
            clippy::cast_precision_loss,
            reason = "Freshness scoring is informational; f64 is adequate for UX metrics"
        )]
        let score = (self.freshness.fresh as f64 * 1.0)
            + (self.freshness.recent as f64 * 0.8)
            + (self.freshness.stale as f64 * 0.5)
            + (self.freshness.very_stale as f64 * 0.2)
            + (self.freshness.never_indexed as f64 * 0.0);

        score / self.total_repos as f64
    }

    /// Get a human-readable health status.
    #[must_use]
    pub fn health_status(&self) -> &'static str {
        let score = self.health_score();
        match score {
            s if s >= 0.9 => "Excellent",
            s if s >= 0.7 => "Good",
            s if s >= 0.5 => "Fair",
            s if s >= 0.3 => "Poor",
            _ => "Critical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::WorkspaceRepoId;
    use std::path::PathBuf;

    fn create_test_repo(name: &str, indexed_ago: Option<Duration>) -> WorkspaceRepository {
        let last_indexed_at = indexed_ago.map(|duration| SystemTime::now() - duration);
        WorkspaceRepository {
            id: WorkspaceRepoId::new(name),
            name: name.to_string(),
            root: PathBuf::from(format!("/workspace/{name}")),
            index_path: PathBuf::from(format!("/workspace/{name}/.sqry-index")),
            last_indexed_at,
            symbol_count: if indexed_ago.is_some() {
                Some(100)
            } else {
                None
            },
            primary_language: Some("rust".to_string()),
        }
    }

    #[test]
    fn test_freshness_buckets() {
        let mut registry = WorkspaceRegistry::new(Some("Test".into()));

        // Fresh (< 1 hour)
        registry
            .upsert_repo(create_test_repo("fresh", Some(Duration::from_secs(1800))))
            .unwrap();

        // Recent (< 1 day)
        registry
            .upsert_repo(create_test_repo("recent", Some(Duration::from_secs(7200))))
            .unwrap();

        // Stale (< 1 week)
        registry
            .upsert_repo(create_test_repo(
                "stale",
                Some(Duration::from_secs(172_800)),
            ))
            .unwrap();

        // Very stale (> 1 week)
        registry
            .upsert_repo(create_test_repo(
                "very-stale",
                Some(Duration::from_secs(691_200)),
            ))
            .unwrap();

        // Never indexed
        registry
            .upsert_repo(create_test_repo("never", None))
            .unwrap();

        let stats = DetailedWorkspaceStats::from_registry(&registry);

        assert_eq!(stats.freshness.fresh, 1);
        assert_eq!(stats.freshness.recent, 1);
        assert_eq!(stats.freshness.stale, 1);
        assert_eq!(stats.freshness.very_stale, 1);
        assert_eq!(stats.freshness.never_indexed, 1);
        assert_eq!(stats.total_repos, 5);
        assert_eq!(stats.indexed_repos, 4);
        assert_eq!(stats.unindexed_repos, 1);
    }

    #[test]
    fn test_health_score() {
        let mut registry = WorkspaceRegistry::new(Some("Test".into()));

        // All fresh repos
        for i in 0..5 {
            registry
                .upsert_repo(create_test_repo(
                    &format!("repo-{i}"),
                    Some(Duration::from_secs(1800)),
                ))
                .unwrap();
        }

        let stats = DetailedWorkspaceStats::from_registry(&registry);
        assert!(stats.health_score() >= 0.9);
        assert_eq!(stats.health_status(), "Excellent");
    }

    #[test]
    fn test_stale_repos() {
        let mut registry = WorkspaceRegistry::new(Some("Test".into()));

        // Fresh repo
        registry
            .upsert_repo(create_test_repo("fresh", Some(Duration::from_secs(1800))))
            .unwrap();

        // Stale repo (3 days old)
        registry
            .upsert_repo(create_test_repo(
                "stale",
                Some(Duration::from_secs(259_200)),
            ))
            .unwrap();

        let stats = DetailedWorkspaceStats::from_registry(&registry);
        let stale = stats.stale_repos(&registry, Duration::from_secs(86400)); // 1 day threshold

        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].name, "stale");
    }
}
