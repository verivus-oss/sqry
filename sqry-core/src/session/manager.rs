//! Session cache manager for warm multi-query execution.
//!
//! The implementation keeps recently-used unified code graphs in-memory
//! so subsequent queries avoid disk deserialization. Filesystem watching
//! ensures cache entries are invalidated automatically when the on-disk
//! graph changes. A background maintenance thread processes file watcher
//! events and reclaims idle cache entries.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use log::{debug, info};

use crate::graph::CodeGraph;
use crate::graph::unified::persistence::{GraphStorage, load_from_path};
use crate::plugin::PluginManager;
use crate::query::QueryExecutor;

use super::cache::CachedIndex;
use super::watcher::FileWatcher;
use super::{SessionConfig, SessionError, SessionResult};

/// Manages cached unified code graphs for a workspace session.
pub struct SessionManager {
    cache: Arc<DashMap<PathBuf, CachedIndex>>,
    config: SessionConfig,
    watcher: Arc<Mutex<FileWatcher>>,
    cleanup_handle: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    total_queries: Arc<AtomicU64>,
    cache_hits: Arc<AtomicU64>,
    cache_misses: Arc<AtomicU64>,
    /// Query executor with properly configured plugin manager
    query_executor: QueryExecutor,
}

impl SessionManager {
    /// Create a session manager with default configuration and an empty plugin manager.
    ///
    /// **Note:** For proper query execution with language support, use
    /// [`with_plugin_manager()`](Self::with_plugin_manager) instead, passing a
    /// `PluginManager` configured with the language plugins you need.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the background watcher thread cannot be spawned or when
    /// watcher initialization fails.
    pub fn new() -> SessionResult<Self> {
        Self::with_config_and_plugins(SessionConfig::default(), PluginManager::new())
    }

    /// Create a session manager with a pre-configured plugin manager.
    ///
    /// This is the recommended constructor for production use. The `PluginManager`
    /// should be created via `sqry_plugin_registry::create_plugin_manager()` or
    /// configured with the language plugins you need for query evaluation.
    ///
    /// # Arguments
    ///
    /// * `plugin_manager` - A `PluginManager` configured with language plugins
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when watcher initialization or thread spawning fails.
    pub fn with_plugin_manager(plugin_manager: PluginManager) -> SessionResult<Self> {
        Self::with_config_and_plugins(SessionConfig::default(), plugin_manager)
    }

    /// Create a session manager with the provided configuration.
    ///
    /// **Note:** This uses an empty plugin manager. For proper query execution,
    /// use [`with_config_and_plugins()`](Self::with_config_and_plugins) instead.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when watcher initialization or thread spawning fails.
    pub fn with_config(config: SessionConfig) -> SessionResult<Self> {
        Self::with_config_and_plugins(config, PluginManager::new())
    }

    /// Create a session manager with configuration and a pre-configured plugin manager.
    ///
    /// This is the most flexible constructor, allowing full control over both
    /// session configuration and plugin setup.
    ///
    /// # Arguments
    ///
    /// * `config` - Session configuration (cache size, timeouts, etc.)
    /// * `plugin_manager` - A `PluginManager` configured with language plugins
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when watcher initialization or thread spawning fails.
    pub fn with_config_and_plugins(
        config: SessionConfig,
        plugin_manager: PluginManager,
    ) -> SessionResult<Self> {
        let watcher = if config.enable_file_watching {
            FileWatcher::new()?
        } else {
            FileWatcher::disabled()
        };

        let cache = Arc::new(DashMap::new());
        let watcher = Arc::new(Mutex::new(watcher));
        let shutdown = Arc::new(AtomicBool::new(false));

        let cleanup_interval = config.cleanup_interval;
        let idle_timeout = config.idle_timeout;

        let cleanup_handle = {
            let cache = Arc::clone(&cache);
            let watcher_clone = Arc::clone(&watcher);
            let shutdown_flag = Arc::clone(&shutdown);

            thread::Builder::new()
                .name("sqry-session-cleanup".into())
                .spawn(move || {
                    while !shutdown_flag.load(Ordering::Relaxed) {
                        if let Ok(mut guard) = watcher_clone.lock()
                            && let Err(err) = guard.process_events()
                        {
                            debug!("failed to process session watcher events: {err}");
                        }

                        let now = Instant::now();
                        if let Err(err) =
                            Self::evict_stale_for(&cache, &watcher_clone, idle_timeout, now)
                        {
                            debug!("failed to evict stale sessions: {err}");
                        }

                        thread::park_timeout(cleanup_interval);
                    }

                    if let Ok(mut guard) = watcher_clone.lock()
                        && let Err(err) = guard.process_events()
                    {
                        debug!("failed to process session watcher events during shutdown: {err}");
                    }
                    let now = Instant::now();
                    if let Err(err) =
                        Self::evict_stale_for(&cache, &watcher_clone, idle_timeout, now)
                    {
                        debug!("failed to evict stale sessions during shutdown: {err}");
                    }
                })
                .map_err(SessionError::SpawnThread)?
        };

        // Create query executor with the provided plugin manager
        let query_executor = QueryExecutor::with_plugin_manager(plugin_manager);

        Ok(Self {
            cache,
            config,
            watcher,
            cleanup_handle: Some(cleanup_handle),
            shutdown,
            total_queries: Arc::new(AtomicU64::new(0)),
            cache_hits: Arc::new(AtomicU64::new(0)),
            cache_misses: Arc::new(AtomicU64::new(0)),
            query_executor,
        })
    }

    /// Get the cached code graph for `path`, loading it from disk on demand.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the graph cannot be loaded.
    pub fn get_graph(&self, path: &Path) -> SessionResult<Arc<CodeGraph>> {
        self.get_or_load_graph(path)
    }

    /// Query symbols from the cached graph for `path`.
    ///
    /// This method loads the graph on demand and returns symbols that match
    /// the query criteria. The query string is parsed and evaluated against
    /// all symbols in the graph.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the graph cannot be loaded, query parsing fails,
    /// or query execution fails.
    pub fn query(
        &self,
        path: &Path,
        query_str: &str,
    ) -> SessionResult<crate::query::results::QueryResults> {
        // Use execute_on_graph which handles graph loading and caching internally
        let results = self
            .query_executor
            .execute_on_graph(query_str, path)
            .map_err(|e| {
                // Determine if this is a parse error or execution error
                let error_msg = e.to_string();
                if error_msg.contains("parse")
                    || error_msg.contains("unexpected")
                    || error_msg.contains("expected")
                {
                    SessionError::QueryParse(error_msg)
                } else {
                    SessionError::QueryExecution(e)
                }
            })?;

        Ok(results)
    }

    /// Remove the cached graph for `path`, if present.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the file watcher fails to unwatch the path.
    pub fn invalidate(&self, path: &Path) -> SessionResult<()> {
        self.cache.remove(path);
        if let Ok(mut watcher) = self.watcher.lock() {
            watcher.unwatch(path)?;
        }
        Ok(())
    }

    /// Ensure the graph for `path` is loaded into the session cache without
    /// affecting query statistics.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when loading from disk fails.
    pub fn preload(&self, path: &Path) -> SessionResult<()> {
        if self.cache.contains_key(path) {
            return Ok(());
        }

        if self.cache.len() >= self.config.max_cached_indexes {
            self.evict_lru();
        }

        self.load_graph_from_disk(path)?;
        Ok(())
    }

    /// Return aggregate session statistics.
    #[must_use]
    pub fn stats(&self) -> SessionStats {
        SessionStats {
            cached_graphs: self.cache.len(),
            total_queries: self.total_queries.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            total_memory_mb: self.cache.len() * 51,
        }
    }

    /// Evict entries that have been idle longer than configured timeout.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when watcher operations fail while removing paths.
    pub fn evict_stale(&self, now: Instant) -> SessionResult<usize> {
        Self::evict_stale_for(&self.cache, &self.watcher, self.config.idle_timeout, now)
    }

    fn get_or_load_graph(&self, path: &Path) -> SessionResult<Arc<CodeGraph>> {
        debug_assert!(
            self.config.max_cached_indexes > 0,
            "SessionConfig::max_cached_indexes must be at least 1"
        );
        self.total_queries.fetch_add(1, Ordering::Relaxed);

        if let Some(entry) = self.cache.get_mut(path) {
            entry.value().access();
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(Arc::clone(&entry.value().graph));
        }

        self.cache_misses.fetch_add(1, Ordering::Relaxed);

        if self.cache.len() >= self.config.max_cached_indexes {
            self.evict_lru();
        }

        let graph = self.load_graph_from_disk(path)?;

        if let Some(entry) = self.cache.get_mut(path) {
            entry.value().access();
        }

        Ok(graph)
    }

    fn load_graph_from_disk(&self, path: &Path) -> SessionResult<Arc<CodeGraph>> {
        let storage = GraphStorage::new(path);
        let snapshot_path = storage.snapshot_path();

        let metadata =
            fs::metadata(snapshot_path).map_err(|source| SessionError::IndexMetadata {
                path: snapshot_path.to_path_buf(),
                source,
            })?;
        let file_mtime = metadata
            .modified()
            .map_err(|source| SessionError::IndexMetadata {
                path: snapshot_path.to_path_buf(),
                source,
            })?;

        let graph = load_from_path(snapshot_path, Some(self.query_executor.plugin_manager()))
            .map_err(|source| SessionError::IndexLoad {
                path: snapshot_path.to_path_buf(),
                source: source.into(),
            })?;
        let arc_graph = Arc::new(graph);

        let cached = CachedIndex::new(Arc::clone(&arc_graph), file_mtime);
        self.cache.insert(path.to_path_buf(), cached);

        self.register_watcher(path, snapshot_path);

        Ok(arc_graph)
    }

    fn register_watcher(&self, workspace_path: &Path, snapshot_path: &Path) {
        if let Ok(mut watcher) = self.watcher.lock() {
            let cache = Arc::clone(&self.cache);
            let callback_path = workspace_path.to_path_buf();
            let watch_path = snapshot_path.to_path_buf();
            if let Err(err) = watcher.watch(watch_path.clone(), move || {
                cache.remove(&callback_path);
                info!(
                    "Invalidated session cache after graph change: {}",
                    callback_path.display()
                );
            }) {
                debug!(
                    "failed to register file watcher for {}: {err}",
                    watch_path.display()
                );
            }
        }
    }

    /// Evict least-recently-used entry when cache is at capacity.
    fn evict_lru(&self) {
        let mut oldest: Option<(PathBuf, Instant)> = None;

        for entry in self.cache.iter() {
            let last_accessed = entry.value().last_accessed();
            if oldest
                .as_ref()
                .is_none_or(|(_, instant)| last_accessed < *instant)
            {
                oldest = Some((entry.key().clone(), last_accessed));
            }
        }

        if let Some((path, _)) = oldest {
            self.cache.remove(&path);
            debug!("evicted LRU session cache entry: {}", path.display());
            if let Ok(mut watcher) = self.watcher.lock() {
                let storage = GraphStorage::new(&path);
                if let Err(err) = watcher.unwatch(storage.snapshot_path()) {
                    debug!("failed to unwatch session path {}: {err}", path.display());
                }
            }
        }
    }

    fn evict_stale_for(
        cache: &Arc<DashMap<PathBuf, CachedIndex>>,
        watcher: &Arc<Mutex<FileWatcher>>,
        timeout: Duration,
        now: Instant,
    ) -> SessionResult<usize> {
        let mut to_remove = Vec::new();

        for entry in cache.iter() {
            let idle = now.duration_since(entry.value().last_accessed());
            if idle > timeout {
                to_remove.push(entry.key().clone());
            }
        }

        for path in &to_remove {
            cache.remove(path);
        }

        if !to_remove.is_empty()
            && let Ok(mut guard) = watcher.lock()
        {
            for path in &to_remove {
                let storage = GraphStorage::new(path);
                guard.unwatch(storage.snapshot_path())?;
            }
        }

        Ok(to_remove.len())
    }

    fn stop_worker(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);

        if let Some(handle) = self.cleanup_handle.take() {
            handle.thread().unpark();
            if let Err(err) = handle.join() {
                debug!("session cleanup thread terminated with error: {err:?}");
            }
        }
    }

    /// Gracefully shut down the cleanup thread. Optional, as [`Drop`] handles it.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when shutting down watcher operations fails.
    pub fn shutdown(mut self) -> SessionResult<()> {
        self.stop_worker();
        Ok(())
    }
}

impl Drop for SessionManager {
    fn drop(&mut self) {
        self.stop_worker();
    }
}

/// High-level statistics describing the current session cache.
#[derive(Debug, Clone)]
pub struct SessionStats {
    /// Number of cached graphs retained in-memory.
    pub cached_graphs: usize,
    /// Total number of graph accesses via this manager.
    pub total_queries: u64,
    /// Number of accesses served from cache.
    pub cache_hits: u64,
    /// Number of accesses that triggered a load from disk.
    pub cache_misses: u64,
    /// Estimated total memory footprint in megabytes.
    pub total_memory_mb: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::persistence::save_to_path;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::tempdir;

    fn write_empty_graph(dir: &Path) -> SessionResult<()> {
        let storage = GraphStorage::new(dir);
        fs::create_dir_all(storage.graph_dir()).map_err(|source| SessionError::IndexMetadata {
            path: storage.graph_dir().to_path_buf(),
            source,
        })?;
        let graph = CodeGraph::new();
        save_to_path(&graph, storage.snapshot_path()).map_err(|source| {
            SessionError::IndexLoad {
                path: storage.snapshot_path().to_path_buf(),
                source: source.into(),
            }
        })?;
        Ok(())
    }

    fn watcher_timeout() -> Duration {
        let base = if cfg!(target_os = "macos") {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(2)
        };

        if std::env::var("CI").is_ok() {
            base * 2
        } else {
            base
        }
    }

    fn background_timeout() -> Duration {
        let base = if cfg!(target_os = "macos") {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(3)
        };

        if std::env::var("CI").is_ok() {
            base * 2
        } else {
            base
        }
    }

    fn wait_until<F>(timeout: Duration, mut predicate: F) -> bool
    where
        F: FnMut() -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn get_graph_loads_and_updates_stats() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let manager = SessionManager::new().unwrap();

        let graph = manager.get_graph(temp.path()).unwrap();
        assert_eq!(graph.snapshot().nodes().len(), 0);

        let stats = manager.stats();
        assert_eq!(stats.total_queries, 1);
        assert_eq!(stats.cache_misses, 1);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cached_graphs, 1);

        manager.shutdown().unwrap();
    }

    #[test]
    fn get_graph_missing_returns_error() {
        let temp = tempdir().unwrap();

        let manager = SessionManager::new().unwrap();
        let err = manager
            .get_graph(temp.path())
            .expect_err("get_graph should fail without graph");

        assert!(matches!(err, SessionError::IndexMetadata { .. }));
        manager.shutdown().unwrap();
    }

    #[test]
    fn preload_does_not_affect_stats() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let manager = SessionManager::new().unwrap();
        manager.preload(temp.path()).unwrap();

        let stats = manager.stats();
        assert_eq!(stats.total_queries, 0);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 0);
        assert_eq!(stats.cached_graphs, 1);

        manager.get_graph(temp.path()).unwrap();
        let after = manager.stats();
        assert_eq!(after.total_queries, 1);
        assert_eq!(after.cache_hits, 1);
        assert_eq!(after.cache_misses, 0);
        assert_eq!(after.cached_graphs, 1);

        manager.shutdown().unwrap();
    }

    #[test]
    fn second_access_hits_cache() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let manager = SessionManager::new().unwrap();

        manager.get_graph(temp.path()).unwrap();
        manager.get_graph(temp.path()).unwrap();

        let stats = manager.stats();
        assert_eq!(stats.total_queries, 2);
        assert_eq!(stats.cache_misses, 1);
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cached_graphs, 1);

        manager.shutdown().unwrap();
    }

    #[test]
    fn concurrent_access_shares_cache() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let manager = Arc::new(SessionManager::new().unwrap());
        let path = temp.path().to_path_buf();

        let handles: Vec<_> = (0..6)
            .map(|_| {
                let mgr = Arc::clone(&manager);
                let path = path.clone();
                thread::spawn(move || {
                    mgr.get_graph(&path).unwrap();
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        let manager = Arc::into_inner(manager).expect("no outstanding references");
        manager.shutdown().unwrap();
    }

    #[test]
    fn invalidate_removes_cached_graph() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let manager = SessionManager::new().unwrap();
        manager.get_graph(temp.path()).unwrap();
        assert_eq!(manager.stats().cached_graphs, 1);

        manager.invalidate(temp.path()).unwrap();
        assert_eq!(manager.stats().cached_graphs, 0);

        manager.shutdown().unwrap();
    }

    #[test]
    fn lru_eviction_removes_oldest_entry() {
        let temp = tempdir().unwrap();
        let base = temp.path();

        let config = SessionConfig {
            max_cached_indexes: 2,
            ..SessionConfig::default()
        };
        let manager = SessionManager::with_config(config).unwrap();

        let repo1 = base.join("repo1");
        let repo2 = base.join("repo2");
        let repo3 = base.join("repo3");
        write_empty_graph(&repo1).unwrap();
        write_empty_graph(&repo2).unwrap();
        write_empty_graph(&repo3).unwrap();

        manager.get_graph(&repo1).unwrap();
        manager.get_graph(&repo2).unwrap();

        // Access repo2 again so repo1 stays LRU.
        manager.get_graph(&repo2).unwrap();
        manager.get_graph(&repo3).unwrap();

        assert_eq!(manager.stats().cached_graphs, 2);
        assert!(manager.cache.contains_key(&repo2));
        assert!(manager.cache.contains_key(&repo3));
        assert!(!manager.cache.contains_key(&repo1));

        manager.shutdown().unwrap();
    }

    #[test]
    fn evict_stale_purges_idle_entries() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let config = SessionConfig {
            idle_timeout: Duration::from_millis(100),
            cleanup_interval: Duration::from_secs(3600),
            ..SessionConfig::default()
        };
        let manager = SessionManager::with_config(config).unwrap();

        manager.get_graph(temp.path()).unwrap();
        assert_eq!(manager.stats().cached_graphs, 1);

        // Simulate last access in the past to avoid sleeping too long.
        if let Some(entry) = manager.cache.get(temp.path()) {
            entry.value().set_last_accessed(
                Instant::now()
                    .checked_sub(Duration::from_millis(200))
                    .unwrap(),
            );
        }

        let evicted = manager.evict_stale(Instant::now()).unwrap();
        assert_eq!(evicted, 1);
        assert_eq!(manager.stats().cached_graphs, 0);

        manager.shutdown().unwrap();
    }

    #[test]
    #[ignore = "flaky: timing-sensitive file watcher test"]
    fn file_changes_trigger_invalidation() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let manager = SessionManager::new().unwrap();
        manager.get_graph(temp.path()).unwrap();
        assert!(manager.cache.contains_key(temp.path()));

        let storage = GraphStorage::new(temp.path());
        std::fs::write(storage.snapshot_path(), b"modified").unwrap();

        let evicted = wait_until(watcher_timeout(), || {
            manager
                .watcher
                .lock()
                .expect("watcher lock poisoned in test")
                .process_events()
                .expect("watcher failed to process events in test");
            !manager.cache.contains_key(temp.path())
        });
        assert!(evicted, "expected watcher to invalidate cache entry");

        manager.shutdown().unwrap();
    }

    #[test]
    #[ignore = "flaky: timing-sensitive background thread test"]
    fn background_thread_processes_watcher_events() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let config = SessionConfig {
            cleanup_interval: Duration::from_millis(50),
            ..SessionConfig::default()
        };
        let manager = SessionManager::with_config(config).unwrap();

        manager.get_graph(temp.path()).unwrap();
        let storage = GraphStorage::new(temp.path());
        std::fs::write(storage.snapshot_path(), b"changed").unwrap();

        let evicted = wait_until(background_timeout(), || {
            !manager.cache.contains_key(temp.path())
        });
        assert!(
            evicted,
            "background thread failed to remove watcher-invalidated entry"
        );

        manager.shutdown().unwrap();
    }

    #[test]
    fn background_thread_evicts_idle_entries() {
        let temp = tempdir().unwrap();
        write_empty_graph(temp.path()).unwrap();

        let config = SessionConfig {
            idle_timeout: Duration::from_millis(50),
            cleanup_interval: Duration::from_millis(30),
            ..SessionConfig::default()
        };
        let manager = SessionManager::with_config(config).unwrap();

        manager.get_graph(temp.path()).unwrap();
        assert_eq!(manager.stats().cached_graphs, 1);

        if let Some(entry) = manager.cache.get(temp.path()) {
            entry.value().set_last_accessed(
                Instant::now()
                    .checked_sub(Duration::from_millis(200))
                    .unwrap(),
            );
        }

        let evicted = wait_until(background_timeout(), || {
            !manager.cache.contains_key(temp.path())
        });
        assert!(
            evicted,
            "background eviction thread did not remove idle entry"
        );

        manager.shutdown().unwrap();
    }
}
