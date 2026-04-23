//! `SqrydHook` — post-publish persistence hook.
//!
//! Phase 6c of the sqryd plan. Every successful publish should
//! trigger a best-effort write of the derived-analysis cache
//! (`.sqry/graph/derived.sqry`) via
//! `sqry_db::persistence::save_derived`, on a background tokio
//! task with a configurable timeout. Errors and timeouts are
//! logged at WARN and absorbed — they never fail the query path.
//!
//! This module defines the trait + a [`NoOpHook`] default impl.
//! The production [`QueryDbHook`] implementation lives here too
//! but gates its `sqry-db` dependency under the `sqry-db-hook`
//! feature so sqry-daemon downstream consumers can pick "no
//! persistence" (unit-test embedding) or "full persistence"
//! (Task 9 daemon binary).
//!
//! The hook runs on the current tokio runtime via
//! `tokio::spawn`; the publish call site does not await it. A
//! 5-second idle timeout (from `DaemonConfig::rebuild_drain_timeout_ms`,
//! clamped by the call site) kills an unresponsive save and
//! logs a single WARN.

use std::{path::Path, sync::Arc, time::Duration};

use sqry_core::graph::CodeGraph;
use tracing::warn;

/// Signature for a post-publish persistence hook.
///
/// Called from [`super::WorkspaceManager::publish_and_retain`]
/// *after* the admission commit has succeeded, with the published
/// `Arc<CodeGraph>` and the workspace root path. The hook is
/// expected to return immediately — any actual IO should be
/// spawned on a tokio task via [`spawn_hook`] or equivalent —
/// because `publish_and_retain` is a sync critical section.
pub trait SqrydHook: Send + Sync + std::fmt::Debug {
    /// Notify the hook that a fresh graph has been published for
    /// `workspace_root`. Implementations should NOT block; they
    /// should spawn a background task and return.
    fn on_publish(&self, workspace_root: &Path, graph: Arc<CodeGraph>);
}

impl<T: SqrydHook + ?Sized> SqrydHook for Arc<T> {
    fn on_publish(&self, workspace_root: &Path, graph: Arc<CodeGraph>) {
        (**self).on_publish(workspace_root, graph);
    }
}

/// Null implementation — used by unit tests + the Phase 6c
/// default when no production hook is wired. Logs nothing, does
/// nothing, adds no runtime overhead.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoOpHook;

impl SqrydHook for NoOpHook {
    fn on_publish(&self, _workspace_root: &Path, _graph: Arc<CodeGraph>) {
        // deliberately empty
    }
}

/// Shared handle to the active hook. The manager stores an
/// `ArcSwap<Arc<dyn SqrydHook>>` so Task 9 can install the
/// production hook after the daemon boots (once the sqry-db
/// `QueryDb` is built), and unit tests can install a recording
/// hook at construction time.
pub type SharedHook = Arc<dyn SqrydHook>;

/// Convenience constructor for [`NoOpHook`] as a [`SharedHook`].
#[must_use]
pub fn noop_hook() -> SharedHook {
    Arc::new(NoOpHook)
}

/// Spawn an async persistence task with the configured timeout.
///
/// The task's result is never awaited by the caller. Errors and
/// timeouts are logged at WARN; the query path is unaffected.
///
/// This helper is public so the production
/// [`super::manager::WorkspaceManager`] and custom `SqrydHook`
/// impls can share the same timeout-and-absorb pattern.
pub fn spawn_hook<F, Fut, E>(
    timeout: Duration,
    workspace_root: std::path::PathBuf,
    task_label: &'static str,
    fut_factory: F,
) where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    tokio::spawn(async move {
        let fut = fut_factory();
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                warn!(
                    task = task_label,
                    workspace = %workspace_root.display(),
                    error = %err,
                    "sqryd hook {task_label} failed (absorbed; query path continues)",
                );
            }
            Err(_elapsed) => {
                warn!(
                    task = task_label,
                    workspace = %workspace_root.display(),
                    timeout_ms = timeout.as_millis() as u64,
                    "sqryd hook {task_label} timed out (absorbed; query path continues)",
                );
            }
        }
    });
}

/// Recording hook used by unit tests to observe hook invocations
/// without exercising the real persistence path.
#[doc(hidden)]
#[derive(Debug, Default)]
pub struct RecordingHook {
    pub invocations: parking_lot::Mutex<Vec<std::path::PathBuf>>,
}

impl RecordingHook {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    #[must_use]
    pub fn invocation_count(&self) -> usize {
        self.invocations.lock().len()
    }

    #[must_use]
    pub fn invocation_roots(&self) -> Vec<std::path::PathBuf> {
        self.invocations.lock().clone()
    }
}

impl SqrydHook for RecordingHook {
    fn on_publish(&self, workspace_root: &Path, _graph: Arc<CodeGraph>) {
        self.invocations.lock().push(workspace_root.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_hook_compiles_through_shared_dispatch() {
        let hook: SharedHook = noop_hook();
        let graph = Arc::new(CodeGraph::new());
        hook.on_publish(Path::new("/repos/example"), graph);
    }

    #[test]
    fn recording_hook_captures_invocations_in_order() {
        let hook = RecordingHook::new();
        let graph = Arc::new(CodeGraph::new());
        hook.on_publish(Path::new("/repos/a"), Arc::clone(&graph));
        hook.on_publish(Path::new("/repos/b"), Arc::clone(&graph));
        assert_eq!(hook.invocation_count(), 2);
        let roots = hook.invocation_roots();
        assert_eq!(roots[0], Path::new("/repos/a"));
        assert_eq!(roots[1], Path::new("/repos/b"));
    }

    #[tokio::test]
    async fn spawn_hook_absorbs_error() {
        // Hook returns Err; timeout wrapper logs at WARN and
        // absorbs. Success criterion: the spawned task completes
        // without panic.
        spawn_hook::<_, _, &'static str>(
            Duration::from_millis(100),
            std::path::PathBuf::from("/repos/example"),
            "test-hook",
            || async { Err("simulated failure") },
        );
        // Give the spawned task time to run.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn spawn_hook_absorbs_timeout() {
        spawn_hook::<_, _, &'static str>(
            Duration::from_millis(10),
            std::path::PathBuf::from("/repos/example"),
            "test-hook",
            || async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(())
            },
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
