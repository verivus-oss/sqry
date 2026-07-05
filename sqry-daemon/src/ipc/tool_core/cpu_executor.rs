//! Bounded CPU executor for daemon read-path tool work (issue #503 Phase 2).
//!
//! Formalizes the `spawn_blocking` + timeout + fire-and-forget-cancel bridge
//! that `tool_core::execute_with_timeout` and the standalone
//! `sqry-mcp::SqryServer::execute_tool_with_timeout` implement by hand, and
//! backs it with a **dedicated bounded Rayon pool** sized to `num_cpus`, so
//! daemon CPU work stops contending unfairly on the single global Rayon pool.
//!
//! # Bridge shape (why `spawn_fifo` + `oneshot`, not `spawn_blocking` + `install`)
//!
//! A `spawn_blocking(|| pool.install(work))` bridge would hold a Tokio
//! blocking thread for the whole time the work is queued and running, so with
//! more than `num_cpus` requests in flight the excess `install` calls sit
//! blocked on Tokio blocking threads: the executor would not be separate from
//! the `max_blocking_threads(64)` cap, and a request whose deadline fires
//! while its work is still queued could not observe the flipped token.
//!
//! Instead `run` submits the closure to the pool with
//! [`rayon::ThreadPool::spawn_fifo`] and `await`s a [`tokio::sync::oneshot`]
//! for the result. While the task is queued or running on a pool worker the
//! caller is a cheap suspended future holding no blocking thread. On deadline
//! the caller flips the shared [`CancellationToken`] (fire-and-forget) and
//! returns `ToolTimeout`; a cancellation-aware body observes the flip at its
//! next poll and returns, a non-cancellable body runs to natural completion on
//! its worker (bounded by its own query cost). Either way the async side is
//! freed and no blocking thread is pinned.
//!
//! # Panic safety
//!
//! A panic on a Rayon worker would, with no pool panic handler, abort the
//! process. The closure is wrapped in
//! [`std::panic::catch_unwind`]/[`std::panic::AssertUnwindSafe`] so a panicking
//! tool becomes [`DaemonError::Internal`] (the same class the `spawn_blocking`
//! join-error path produces), never a daemon abort.

use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sqry_core::query::cancellation::CancellationToken;

use crate::error::DaemonError;

/// Fallback pool size when `available_parallelism()` cannot be determined.
const DEFAULT_CPU_THREADS_FALLBACK: usize = 4;

/// Dedicated bounded CPU executor. Cheap to `clone` (it is `Arc`-backed).
///
/// The type is `pub` only so the `pub` daemon-hosting entrypoints
/// (`DaemonMcpHandler::new`, `host_mcp_on_streams`) can name it in their
/// signatures; its constructors and `run` are `pub(crate)`, so it is a
/// daemon-internal type in practice.
#[derive(Clone)]
pub struct CpuExecutor {
    pool: Arc<rayon::ThreadPool>,
    threads: usize,
}

impl std::fmt::Debug for CpuExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuExecutor")
            .field("threads", &self.threads)
            .finish()
    }
}

/// Number of dedicated CPU-pool threads: `available_parallelism()` with a
/// small fixed fallback (never zero).
pub(crate) fn default_cpu_threads() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(DEFAULT_CPU_THREADS_FALLBACK)
}

impl CpuExecutor {
    /// Construct with a pool sized from [`default_cpu_threads`].
    pub(crate) fn new() -> Self {
        Self::with_threads(default_cpu_threads())
    }

    /// Construct with an explicit thread count (used by unit tests to assert
    /// the bound). `threads` is clamped to at least 1.
    pub(crate) fn with_threads(threads: usize) -> Self {
        let threads = threads.max(1);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("sqryd-cpu-{i}"))
            .build()
            .expect("build dedicated cpu rayon pool");
        Self {
            pool: Arc::new(pool),
            threads,
        }
    }

    /// Configured pool size. Equals `pool.current_num_threads()`.
    #[cfg(test)]
    pub(crate) fn threads(&self) -> usize {
        self.threads
    }

    /// Submit owned CPU work under a per-tool deadline.
    ///
    /// The closure receives a borrowed [`CancellationToken`] it can poll and
    /// propagate into nested calls. On deadline the token is flipped
    /// (fire-and-forget) and [`DaemonError::ToolTimeout`] is returned without
    /// awaiting the pooled task. A closure error is classified through the
    /// shared [`super::classify_closure_error`] ladder so every wire envelope
    /// (`ToolTimeout`, `QueryTooBroad`, `RpcErrorPreserved`, `Internal`)
    /// matches the pre-Phase-2 `execute_with_timeout` behaviour byte-for-byte.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::ToolTimeout`] on deadline.
    /// - [`DaemonError::Internal`] on a panic in the closure or a dropped
    ///   result channel.
    /// - Whatever [`super::classify_closure_error`] maps the closure's
    ///   `anyhow::Error` to (cost-gate, budget, RpcError, or `Internal`).
    pub(crate) async fn run<F, T>(
        &self,
        tool_timeout: Duration,
        root: &Path,
        f: F,
    ) -> Result<T, DaemonError>
    where
        F: FnOnce(&CancellationToken) -> anyhow::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let deadline_ms = u64::try_from(tool_timeout.as_millis()).unwrap_or(u64::MAX);
        let secs = tool_timeout.as_secs();
        let root_owned = root.to_path_buf();

        // Per-request token: wrapper retains `cancel`, closure owns a clone.
        let cancel = CancellationToken::new();
        let cancel_for_closure = cancel.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();

        // spawn_fifo gives FIFO admission to the pool across concurrent tool
        // requests; the closure runs on a dedicated-pool worker, so any nested
        // `into_par_iter` it invokes uses this pool, not the global one.
        self.pool.spawn_fifo(move || {
            // catch_unwind: a panicking tool must not abort a pool worker.
            let out = catch_unwind(AssertUnwindSafe(|| f(&cancel_for_closure)));
            // Receiver may be gone after a deadline; ignore the send result.
            let _ = tx.send(out);
        });

        match tokio::time::timeout(tool_timeout, rx).await {
            Ok(Ok(Ok(closure_result))) => match closure_result {
                Ok(value) => Ok(value),
                Err(err) => Err(super::classify_closure_error(
                    err,
                    &root_owned,
                    secs,
                    deadline_ms,
                )),
            },
            // Closure panicked (caught): map to Internal, mirroring the
            // spawn_blocking join-error class. Extract the panic message so
            // the `Internal` payload stays informative, matching the
            // pre-Phase-2 path where the `JoinError` Display carried the
            // panic message into the reason text.
            Ok(Ok(Err(panic))) => {
                let msg = panic
                    .downcast_ref::<&'static str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic payload".to_owned());
                Err(DaemonError::Internal(anyhow::anyhow!(
                    "cpu tool panicked: {msg}"
                )))
            }
            // Sender dropped without sending (should not happen: catch_unwind
            // always yields a value to send). Treated as an internal fault.
            Ok(Err(_recv)) => Err(DaemonError::Internal(anyhow::anyhow!(
                "cpu worker dropped the result channel"
            ))),
            // Deadline elapsed: flip the token (fire-and-forget) so a
            // cancellation-aware body stops at its next poll, and return the
            // canonical ToolTimeout envelope.
            Err(_elapsed) => {
                cancel.cancel();
                Err(DaemonError::ToolTimeout {
                    root: root_owned,
                    secs,
                    deadline_ms,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn default_cpu_threads_is_at_least_one() {
        assert!(default_cpu_threads() >= 1);
    }

    #[test]
    fn with_threads_bound_is_enforced() {
        let exec = CpuExecutor::with_threads(3);
        assert_eq!(exec.threads(), 3);
        assert_eq!(exec.pool.current_num_threads(), 3);
        // Clamp: 0 becomes 1.
        assert_eq!(CpuExecutor::with_threads(0).threads(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn happy_path_returns_value() {
        let exec = CpuExecutor::with_threads(2);
        let out: i64 = exec
            .run(Duration::from_secs(5), Path::new("/ws"), |_cancel| {
                Ok(41 + 1)
            })
            .await
            .expect("happy path");
        assert_eq!(out, 42);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn panic_in_closure_maps_to_internal() {
        let exec = CpuExecutor::with_threads(2);
        let err = exec
            .run(
                Duration::from_secs(5),
                Path::new("/ws"),
                |_cancel| -> anyhow::Result<()> {
                    panic!("boom");
                },
            )
            .await
            .expect_err("panic must surface as an error, not abort");
        assert!(
            matches!(err, DaemonError::Internal(_)),
            "panic must map to Internal, got: {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deadline_flips_token_and_returns_tool_timeout() {
        let exec = CpuExecutor::with_threads(2);
        let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_c = Arc::clone(&observed);
        let err = exec
            .run(Duration::from_millis(30), Path::new("/ws"), move |cancel| {
                // Cooperative spin, bounded so a broken test cannot hang.
                for _ in 0..5_000 {
                    if cancel.is_cancelled() {
                        observed_c.store(true, Ordering::SeqCst);
                        return Err(anyhow::Error::new(sqry_core::query::QueryError::Cancelled));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Ok(())
            })
            .await
            .expect_err("deadline must fire");
        match err {
            DaemonError::ToolTimeout {
                secs, deadline_ms, ..
            } => {
                assert_eq!(secs, 0);
                assert_eq!(deadline_ms, 30);
            }
            other => panic!("expected ToolTimeout, got: {other:?}"),
        }
        // The pooled closure observed the flipped token and stopped.
        for _ in 0..500 {
            if observed.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("closure never observed the deadline-flipped token");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrency_is_bounded_by_pool_size() {
        // Submit more spinning closures than the pool has workers and assert
        // at most `threads` run concurrently, while all still complete.
        const N: usize = 8;
        const THREADS: usize = 2;
        let exec = CpuExecutor::with_threads(THREADS);
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..N {
            let e = exec.clone();
            let live_c = Arc::clone(&live);
            let peak_c = Arc::clone(&peak);
            handles.push(tokio::spawn(async move {
                e.run(Duration::from_secs(30), Path::new("/ws"), move |_cancel| {
                    let now = live_c.fetch_add(1, Ordering::SeqCst) + 1;
                    peak_c.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(40));
                    live_c.fetch_sub(1, Ordering::SeqCst);
                    Ok::<_, anyhow::Error>(())
                })
                .await
            }));
        }
        for h in handles {
            h.await.expect("join").expect("run ok");
        }
        assert!(
            peak.load(Ordering::SeqCst) <= THREADS,
            "peak concurrency {} exceeded pool size {THREADS}",
            peak.load(Ordering::SeqCst)
        );
    }
}
