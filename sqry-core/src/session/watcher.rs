//! Filesystem watcher utility for session cache invalidation.
//!
//! Wraps the `notify` crate with a small abstraction that tracks callbacks
//! keyed by the watched file path. Each callback is triggered when the
//! corresponding workspace's `.sqry/graph/manifest.json` file is modified;
//! callers typically use this to invalidate the in-memory cache entry for
//! that workspace. The manifest is the canonical marker emitted by
//! `build_unified_graph_inner` (see
//! `graph/unified/persistence/mod.rs`'s `GRAPH_DIR_NAME` /
//! `MANIFEST_FILE_NAME` constants); the legacy `.sqry-index` placeholder
//! was never written by the live build pipeline.
//!
//! ## RR-10 Gap #3: Bounded Event Queue (`DoS` Prevention)
//!
//! Uses a bounded synchronous channel (`sync_channel`) instead of unbounded
//! channel to prevent memory exhaustion attacks via filesystem event flooding.
//! The queue capacity is configurable via `SQRY_WATCH_EVENT_QUEUE` environment
//! variable (default: 10,000 events).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};

use notify::{
    Config as NotifyConfig, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};

use super::error::{SessionError, SessionResult};
use crate::config::buffers::watch_event_queue_capacity;

/// File name of the canonical sqry graph manifest. The live build pipeline
/// writes `<workspace>/.sqry/graph/manifest.json` via
/// `graph/unified/persistence::GraphStorage`; this watcher matches events
/// whose target leaf name equals this constant and whose parent directory
/// chain is `.sqry/graph/`.
const MANIFEST_FILE_NAME: &str = "manifest.json";

/// Directory segment containing [`MANIFEST_FILE_NAME`].
const GRAPH_DIR_SEGMENT: &str = "graph";

/// Parent directory of [`GRAPH_DIR_SEGMENT`]. The full canonical relative
/// path is `.sqry/graph/manifest.json`.
const SQRY_DIR_SEGMENT: &str = ".sqry";

/// Build the canonical manifest path watched for changes inside `workspace`.
///
/// Returns `<workspace>/.sqry/graph/manifest.json`. Currently only the
/// in-module tests construct paths through this helper; production callers
/// (e.g. `session::manager::register_watcher`) already hold a fully-formed
/// `GraphStorage` path and pass it directly.
#[cfg(test)]
fn manifest_path(workspace: &Path) -> PathBuf {
    workspace
        .join(SQRY_DIR_SEGMENT)
        .join(GRAPH_DIR_SEGMENT)
        .join(MANIFEST_FILE_NAME)
}

type Callback = Arc<dyn Fn() + Send + Sync + 'static>;

struct WatcherState {
    watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<Event>>,
    callbacks: Arc<Mutex<HashMap<PathBuf, Callback>>>,
}

impl WatcherState {
    fn lock_callbacks(&self) -> MutexGuard<'_, HashMap<PathBuf, Callback>> {
        self.callbacks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Lightweight wrapper around `notify` for watching
/// `.sqry/graph/manifest.json` files.
pub struct FileWatcher {
    state: Option<WatcherState>,
}

impl FileWatcher {
    /// Create an active file watcher.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the underlying `notify` watcher cannot be initialised.
    pub fn new() -> SessionResult<Self> {
        // RR-10 Gap #3: Use bounded channel to prevent DoS via event flooding
        // Configurable via SQRY_WATCH_EVENT_QUEUE (default: 10,000)
        let capacity = watch_event_queue_capacity();
        let (tx, rx) = std::sync::mpsc::sync_channel(capacity);

        let watcher = RecommendedWatcher::new(
            move |event| {
                let _ = tx.send(event);
            },
            NotifyConfig::default(),
        )
        .map_err(SessionError::WatcherInit)?;

        Ok(Self {
            state: Some(WatcherState {
                watcher,
                rx,
                callbacks: Arc::new(Mutex::new(HashMap::new())),
            }),
        })
    }

    /// Create a disabled watcher (used when file watching is turned off).
    #[must_use]
    pub fn disabled() -> Self {
        Self { state: None }
    }

    /// Register a path for change notifications.
    ///
    /// When the underlying `.sqry/graph/manifest.json` file changes,
    /// `on_change` is invoked. The `path` argument is forwarded directly to
    /// the underlying `notify` watcher — callers that already know the
    /// manifest's location (e.g. `GraphStorage::manifest_path()`) pass it
    /// here.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the watcher cannot register the path.
    pub fn watch<F>(&mut self, path: PathBuf, on_change: F) -> SessionResult<()>
    where
        F: Fn() + Send + Sync + 'static,
    {
        let Some(state) = &mut self.state else {
            // Watching disabled; nothing to do.
            return Ok(());
        };

        // Avoid duplicate registrations for the same workspace.
        if state.lock_callbacks().contains_key(&path) {
            return Ok(());
        }

        state
            .watcher
            .watch(&path, RecursiveMode::NonRecursive)
            .map_err(|source| SessionError::WatchIndex {
                path: path.clone(),
                source,
            })?;

        state.lock_callbacks().insert(path, Arc::new(on_change));

        Ok(())
    }

    /// Stop watching a path.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the watcher fails to unregister the path.
    pub fn unwatch(&mut self, path: &Path) -> SessionResult<()> {
        let Some(state) = &mut self.state else {
            return Ok(());
        };

        if state.lock_callbacks().remove(path).is_some() {
            state
                .watcher
                .unwatch(path)
                .map_err(|source| SessionError::UnwatchIndex {
                    path: path.to_path_buf(),
                    source,
                })?;
        }

        Ok(())
    }

    /// Returns the set of paths currently registered for change
    /// notifications. Test-only helper used to assert that production
    /// callers wire the watcher to the correct artifact path
    /// (`.sqry/graph/manifest.json`); accessing it from non-test code
    /// would defeat the encapsulation of the callback table.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn watched_paths(&self) -> Vec<PathBuf> {
        self.state
            .as_ref()
            .map(|state| state.lock_callbacks().keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Invoke the registered callback for `path` without waiting for the
    /// platform file-watcher backend. Test-only so session manager tests can
    /// cover watcher-triggered invalidation deterministically.
    #[cfg(test)]
    pub(crate) fn trigger_for_test(&self, path: &Path) -> bool {
        let callback = self
            .state
            .as_ref()
            .and_then(|state| state.lock_callbacks().get(path).cloned());

        if let Some(callback) = callback {
            callback();
            true
        } else {
            false
        }
    }

    /// Drain pending filesystem events and invoke registered callbacks.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when event processing fails catastrophically (e.g., callback errors).
    pub fn process_events(&mut self) -> SessionResult<()> {
        let Some(state) = &mut self.state else {
            return Ok(());
        };

        loop {
            match state.rx.try_recv() {
                Ok(Ok(event)) => Self::handle_event(state, &event),
                Ok(Err(err)) => {
                    log::warn!("file watcher error: {err}");
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }

        Ok(())
    }

    /// Wait for a duration while processing events
    ///
    /// Unlike `thread::sleep()` followed by `process_events()`, this actively
    /// drains and processes events during the wait period. This is crucial for
    /// macOS `FSEvents` which may deliver batched notifications with higher latency.
    ///
    /// Reference: `CI_FAILURE_REMEDIATION_PLAN.md` Section 2
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the event loop encounters an unrecoverable error.
    pub fn wait_and_process(&mut self, duration: std::time::Duration) -> SessionResult<()> {
        let Some(state) = &mut self.state else {
            return Ok(());
        };

        let deadline = std::time::Instant::now() + duration;

        while std::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let poll_interval = std::time::Duration::from_millis(10).min(remaining);

            match state.rx.recv_timeout(poll_interval) {
                Ok(Ok(event)) => Self::handle_event(state, &event),
                Ok(Err(err)) => {
                    log::warn!("file watcher error: {err}");
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Continue waiting
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
            }
        }

        Ok(())
    }

    fn handle_event(state: &WatcherState, event: &Event) {
        use EventKind::{Any, Create, Modify, Remove};

        let relevant = matches!(event.kind, Modify(_) | Create(_) | Remove(_) | Any);

        if !relevant {
            return;
        }

        // Collect callbacks to invoke outside the mutex.
        let mut callbacks_to_run: Vec<Callback> = Vec::new();

        {
            let callbacks = state.lock_callbacks();
            for path in &event.paths {
                // Only react to writes targeting `.sqry/graph/manifest.json`.
                // We require the full parent chain so unrelated `manifest.json`
                // files (e.g. NPM package manifests) cannot trigger spurious
                // invalidations.
                if path
                    .file_name()
                    .is_some_and(|name| name == OsStr::new(MANIFEST_FILE_NAME))
                    && let Some(graph_dir) = path.parent()
                    && graph_dir
                        .file_name()
                        .is_some_and(|name| name == OsStr::new(GRAPH_DIR_SEGMENT))
                    && let Some(sqry_dir) = graph_dir.parent()
                    && sqry_dir
                        .file_name()
                        .is_some_and(|name| name == OsStr::new(SQRY_DIR_SEGMENT))
                    && let Some(callback) = callbacks.get(path)
                {
                    callbacks_to_run.push(Arc::clone(callback));
                }
            }
        }

        for callback in callbacks_to_run {
            callback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tempfile::tempdir;

    fn event_timeout() -> Duration {
        // CI environments need more generous timeouts due to resource constraints
        let base = if cfg!(target_os = "macos") {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(1) // Increased from 250ms for CI stability
        };

        // Double timeout in CI environment
        if std::env::var("CI").is_ok() {
            base * 2
        } else {
            base
        }
    }

    #[test]
    #[cfg_attr(target_os = "macos", ignore = "FSEvents timing flaky in CI")]
    fn detects_changes_to_index_file() {
        let temp = tempdir().unwrap();
        let workspace = temp.path();
        let manifest = manifest_path(workspace);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, b"initial").unwrap();

        let mut watcher = FileWatcher::new().unwrap();

        let triggered = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&triggered);
        watcher
            .watch(manifest.clone(), move || {
                flag.store(true, Ordering::SeqCst);
            })
            .unwrap();

        std::fs::write(&manifest, b"modified").unwrap();

        watcher.wait_and_process(event_timeout()).unwrap();

        assert!(triggered.load(Ordering::SeqCst));
    }

    #[test]
    fn disabled_watcher_is_noop() {
        let temp = tempdir().unwrap();
        let workspace = temp.path();
        let manifest = manifest_path(workspace);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(&manifest, b"data").unwrap();

        let mut watcher = FileWatcher::disabled();
        watcher
            .watch(manifest, || {
                panic!("disabled watcher should not invoke callback");
            })
            .unwrap();
        // No events should be processed, but method should be callable.
        watcher.process_events().unwrap();
    }
}
