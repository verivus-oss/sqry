//! Workspace statistics and staleness tracking.
//!
//! # Live symbol counts (issue #515 staleness fix)
//!
//! [`DetailedWorkspaceStats::from_registry`] (the path `sqry workspace
//! stats` runs) reads each indexed member's symbol count **live** from
//! its `.sqry/graph/manifest.json` at stats-computation time, via
//! [`WorkspaceRepository::symbol_count_from_manifest`]. It deliberately
//! does not use the registry's cached
//! [`WorkspaceRepository::symbol_count_at_registration`] field, because
//! that value is only refreshed at registration time
//! (`discover_repositories` / `sqry workspace add`) and goes stale the
//! moment a member is reindexed directly (`sqry index --force`) without
//! a matching `workspace remove` + `workspace add` round-trip. `sqry
//! workspace query` already reads member graphs live via
//! `SessionManager`; `stats` now matches that freshness contract instead
//! of trusting a point-in-time snapshot that a reindex can silently
//! invalidate.

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
    /// Total symbol count across all indexed repositories whose
    /// `manifest.json` sidecar could be read. Repositories counted in
    /// `unknown_symbol_count_repos` do not contribute here.
    pub total_symbols: u64,
    /// Freshness buckets categorizing repositories by age.
    pub freshness: FreshnessBuckets,
    /// Average symbols per repository with a known symbol count
    /// (`total_symbols` divided by the repos that contributed to it, not
    /// by `indexed_repos`, so an unreadable manifest cannot silently
    /// drag the average down).
    pub avg_symbols_per_repo: f64,
    /// Indexed repositories (`last_indexed_at.is_some()`) whose
    /// `.sqry/graph/manifest.json` could not be read or parsed, so their
    /// symbol count is unknown rather than zero. Surfaced separately so
    /// `sqry workspace stats` can report them instead of silently
    /// folding them into `total_symbols` as zero contributions.
    pub unknown_symbol_count_repos: usize,
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
    ///
    /// Reads every indexed member's symbol count **live** from its
    /// `.sqry/graph/manifest.json` sidecar (via
    /// [`WorkspaceRepository::symbol_count_from_manifest`]), not from the
    /// registry's cached `symbol_count_at_registration` field. See the
    /// module-level docs for why: this is the fix for issue #515's
    /// staleness gap (stats reporting a stale count after a direct `sqry
    /// index --force` reindex of a member).
    #[must_use]
    pub fn from_registry(registry: &WorkspaceRegistry) -> Self {
        Self::from_registry_with_resolver(registry, |repo| {
            WorkspaceRepository::symbol_count_from_manifest(&repo.root)
        })
    }

    /// Compute detailed statistics from a workspace registry using a
    /// caller-supplied symbol-count resolver instead of always reading
    /// `.sqry/graph/manifest.json` from disk.
    ///
    /// [`Self::from_registry`] is the production entry point (used by
    /// `sqry workspace stats`) and always resolves live from each
    /// member's manifest. This variant exists so the aggregation logic
    /// (freshness split, `total_symbols` sum, `unknown_symbol_count_repos`
    /// bucketing, `avg_symbols_per_repo` denominator) stays unit-testable
    /// without needing real manifest fixtures on disk for every test:
    /// callers can inject a resolver that returns canned known/unknown
    /// counts per repository.
    #[must_use]
    pub fn from_registry_with_resolver(
        registry: &WorkspaceRegistry,
        resolve_symbol_count: impl Fn(&WorkspaceRepository) -> Option<u64>,
    ) -> Self {
        let total_repos = registry.repositories.len();
        let indexed_repos = registry
            .repositories
            .iter()
            .filter(|r| r.last_indexed_at.is_some())
            .count();
        let unindexed_repos = total_repos - indexed_repos;

        // Split indexed repos into "symbol count known" (manifest.json
        // read cleanly) versus "unknown" (manifest missing/corrupt).
        // Repositories that were never indexed at all contribute to
        // neither bucket; they are already accounted for by
        // `unindexed_repos` / `freshness.never_indexed`.
        let mut known_symbol_count_repos = 0usize;
        let mut unknown_symbol_count_repos = 0usize;
        let mut total_symbols: u64 = 0;
        for repo in &registry.repositories {
            if repo.last_indexed_at.is_none() {
                continue;
            }
            match resolve_symbol_count(repo) {
                Some(count) => {
                    total_symbols += count;
                    known_symbol_count_repos += 1;
                }
                None => unknown_symbol_count_repos += 1,
            }
        }

        let avg_symbols_per_repo = if known_symbol_count_repos > 0 {
            u64_to_f64(total_symbols) / usize_to_f64(known_symbol_count_repos)
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
            unknown_symbol_count_repos,
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
    use crate::graph::unified::persistence::{BuildProvenance, Manifest};
    use crate::workspace::WorkspaceRepoId;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn create_test_repo(name: &str, indexed_ago: Option<Duration>) -> WorkspaceRepository {
        let last_indexed_at = indexed_ago.map(|duration| SystemTime::now() - duration);
        WorkspaceRepository {
            id: WorkspaceRepoId::new(name),
            name: name.to_string(),
            root: PathBuf::from(format!("/workspace/{name}")),
            index_path: PathBuf::from(format!("/workspace/{name}/.sqry-index")),
            last_indexed_at,
            symbol_count_at_registration: if indexed_ago.is_some() {
                Some(100)
            } else {
                None
            },
            primary_language: Some("rust".to_string()),
        }
    }

    /// Resolver mirroring the pre-#515-staleness-fix behavior: reads the
    /// registration-time snapshot field directly, with no filesystem
    /// access. Used by tests that only exercise the aggregation logic
    /// (freshness split, `total_symbols` sum, `unknown_symbol_count_repos`
    /// bucketing, `avg_symbols_per_repo` denominator) and don't need a
    /// real `.sqry/graph/manifest.json` fixture on disk.
    fn resolve_from_registration_snapshot(repo: &WorkspaceRepository) -> Option<u64> {
        repo.symbol_count_at_registration
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

    /// Issue #515 edge case: an indexed repo whose resolved symbol count
    /// is `None` (manifest missing/corrupt) must be excluded from
    /// `total_symbols` and the `avg_symbols_per_repo` denominator, and
    /// reported via `unknown_symbol_count_repos`, rather than silently
    /// treated as a zero-symbol repo that drags the average down.
    ///
    /// Uses [`from_registry_with_resolver`] with
    /// [`resolve_from_registration_snapshot`] so it exercises the
    /// aggregation logic in isolation, without needing real
    /// `.sqry/graph/manifest.json` fixtures on disk (that live-read path
    /// is covered separately by
    /// `test_from_registry_reads_symbol_count_live_not_cached` below).
    #[test]
    fn test_unknown_symbol_count_repo_excluded_from_average() {
        let mut registry = WorkspaceRegistry::new(Some("Test".into()));

        // Two indexed repos with known counts.
        registry
            .upsert_repo(create_test_repo("known-a", Some(Duration::from_secs(60))))
            .unwrap();
        registry
            .upsert_repo(create_test_repo("known-b", Some(Duration::from_secs(60))))
            .unwrap();

        // One indexed repo whose manifest could not be read: last_indexed_at
        // is set (mtime of the manifest file was still readable) but the
        // resolved symbol count is None.
        let mut unknown_repo = create_test_repo("unknown", Some(Duration::from_secs(60)));
        unknown_repo.symbol_count_at_registration = None;
        registry.upsert_repo(unknown_repo).unwrap();

        // One never-indexed repo: must not be double-counted into
        // unknown_symbol_count_repos, it already has its own bucket.
        registry
            .upsert_repo(create_test_repo("never", None))
            .unwrap();

        let stats = DetailedWorkspaceStats::from_registry_with_resolver(
            &registry,
            resolve_from_registration_snapshot,
        );

        assert_eq!(stats.total_repos, 4);
        assert_eq!(stats.indexed_repos, 3);
        assert_eq!(stats.unindexed_repos, 1);
        assert_eq!(
            stats.total_symbols, 200,
            "total_symbols must sum only the repos with a known count (100 + 100)"
        );
        assert_eq!(
            stats.unknown_symbol_count_repos, 1,
            "the corrupt-manifest repo must be counted separately, not folded into total_symbols as 0"
        );
        assert!(
            (stats.avg_symbols_per_repo - 100.0).abs() < f64::EPSILON,
            "average must divide by the 2 repos with a known count (200 / 2 = 100), \
             not by all 3 indexed repos (which would wrongly yield 66.67): got {}",
            stats.avg_symbols_per_repo
        );
    }

    /// Issue #515 staleness regression (the cross-LLM gate's blocker on
    /// the original fix): `DetailedWorkspaceStats::from_registry` must
    /// read each member's symbol count *live* from its
    /// `.sqry/graph/manifest.json` at stats-computation time, not from
    /// the registry's `symbol_count_at_registration` snapshot.
    ///
    /// Before this fix, `from_registry` read
    /// `WorkspaceRepository::symbol_count` directly (the same field this
    /// test renamed to `symbol_count_at_registration`), a value only
    /// refreshed by `discover_repositories` / `sqry workspace add`. A
    /// direct `sqry index --force` reindex of a member changes
    /// `.sqry/graph/manifest.json`'s `node_count` without touching the
    /// workspace registry at all, so `stats` reported the old count
    /// forever until the member was removed and re-added, even though
    /// `sqry workspace query` (which loads member graphs live via
    /// `SessionManager`) already reflected the new one.
    ///
    /// This test reindexes the manifest between two `from_registry` calls
    /// with no registry mutation in between (mirroring the exact
    /// reproduction the gate used: 8 -> 18 nodes without `workspace add`
    /// / `remove`) and asserts the second call's `total_symbols` tracks
    /// the new manifest, not the untouched registration-time snapshot.
    /// This fails against the pre-fix code (which would still report 8).
    #[test]
    fn test_from_registry_reads_symbol_count_live_not_cached() {
        let temp = tempdir().unwrap();
        let root = temp.path();

        let repo_dir = root.join("member");
        let graph_dir = repo_dir.join(".sqry/graph");
        std::fs::create_dir_all(&graph_dir).unwrap();

        let write_manifest = |node_count: usize| {
            let manifest = Manifest::new(
                repo_dir.display().to_string(),
                node_count,
                node_count * 2,
                "test-sha256",
                BuildProvenance::new("test", "test"),
            );
            manifest.save(graph_dir.join("manifest.json")).unwrap();
        };

        // Initial index: 8 symbols.
        write_manifest(8);

        let mut registry = WorkspaceRegistry::new(Some("Test".into()));
        let mut repo = WorkspaceRepository::new(
            WorkspaceRepoId::new("member"),
            "member".to_string(),
            repo_dir.clone(),
            graph_dir.join("manifest.json"),
            Some(SystemTime::now()),
        );
        // Registration-time snapshot matches the initial manifest, then
        // is deliberately never touched again for the rest of the test
        // (no `discover_repositories` / `workspace add` re-run), exactly
        // like a real `sqry index --force` reindex that never goes
        // through `workspace add`/`remove`.
        repo.symbol_count_at_registration = Some(8);
        registry.upsert_repo(repo).unwrap();

        let stats_before = DetailedWorkspaceStats::from_registry(&registry);
        assert_eq!(
            stats_before.total_symbols, 8,
            "sanity check: initial stats must match the initial manifest node_count"
        );

        // Reindex the member directly: the manifest's node_count changes
        // to 18, but nothing touches the workspace registry.
        write_manifest(18);

        let stats_after = DetailedWorkspaceStats::from_registry(&registry);
        assert_eq!(
            stats_after.total_symbols, 18,
            "stats must reflect the reindexed manifest's node_count (18) even though \
             the registry's symbol_count_at_registration snapshot is still stuck at 8; \
             this is the issue #515 staleness gap the gate flagged"
        );
        assert_eq!(
            stats_after.unknown_symbol_count_repos, 0,
            "the reindexed manifest is still readable, so the count stays known"
        );
        assert!(
            (stats_after.avg_symbols_per_repo - 18.0).abs() < f64::EPSILON,
            "avg must also reflect the live count: got {}",
            stats_after.avg_symbols_per_repo
        );
    }
}
