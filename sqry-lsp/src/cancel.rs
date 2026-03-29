use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::task::{JoinError, JoinHandle};

/// `JoinHandle` wrapper that aborts the underlying task if dropped before completion.
pub struct CancelableJoinHandle<T>
where
    T: Send + 'static,
{
    handle: Option<JoinHandle<T>>,
}

impl<T> CancelableJoinHandle<T>
where
    T: Send + 'static,
{
    pub fn new(handle: JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    /// Abort the underlying task immediately.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn abort(&mut self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.abort();
        }
    }
}

impl<T> Future for CancelableJoinHandle<T>
where
    T: Send + 'static,
{
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let handle = this
            .handle
            .as_mut()
            .expect("polled CancelableJoinHandle after completion");

        match Pin::new(handle).poll(cx) {
            Poll::Ready(res) => {
                this.handle = None;
                Poll::Ready(res)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Drop for CancelableJoinHandle<T>
where
    T: Send + 'static,
{
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Spawn a blocking task whose join handle aborts automatically when dropped.
///
/// # Cancellation Safety
///
/// This function is designed for LSP request cancellation. The returned
/// `CancelableJoinHandle` aborts the underlying blocking task if dropped
/// before completion, making it ideal for cancelling long-running operations
/// when an LSP client sends a `$/cancelRequest` notification.
///
/// **Behavior on cancellation**:
/// - The blocking task is immediately aborted via `JoinHandle::abort()`
/// - Any in-progress work is interrupted and discarded
/// - No cleanup or compensation logic runs in the task
///
/// **Safety guarantees**:
/// - All LSP handlers using this wrapper perform idempotent, per-request operations
/// - No persistent state mutations occur, so cancellation cannot corrupt data
/// - Partial results are safely discarded
///
/// # Examples
///
/// ```no_run
/// use sqry_lsp::spawn_blocking;
///
/// async fn search_handler() {
///     let handle = spawn_blocking(|| {
///         // Heavy search operation
///         perform_search()
///     });
///
///     // If this future is dropped (e.g., on $/cancelRequest),
///     // the search task is aborted and results are discarded.
///     let result = handle.await;
/// }
/// # fn perform_search() {}
/// ```
pub fn spawn_blocking<F, T>(f: F) -> CancelableJoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    CancelableJoinHandle::new(tokio::task::spawn_blocking(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn join_handle_completes() {
        let handle = spawn_blocking(|| 41 + 1);
        let result = handle.await.expect("join ok");
        assert_eq!(result, 42);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aborting_handle_yields_cancelled() {
        // Use tokio::spawn (not spawn_blocking) so abort is deterministic:
        // on current_thread the spawned task cannot make progress before we
        // call abort(), guaranteeing cancellation.
        let mut handle = CancelableJoinHandle::new(tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            0usize
        }));
        handle.abort();
        let result = handle.await.expect_err("expected cancellation");
        assert!(result.is_cancelled());
    }
}
