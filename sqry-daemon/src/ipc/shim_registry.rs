//! Shim-connection registry.
//!
//! Phase 8a introduced these types as dormant `pub(crate)` scaffolding.
//! Phase 8c (U3) widens the surface to `pub` so:
//!
//! - the router + MCP host can register shim connections as they
//!   attach, and
//! - [`super::server::IpcServer::shim_registry`] can hand the
//!   registry back to Task 9's bootstrap path and `daemon/status` for
//!   connection-count reporting.
//!
//! The byte-pump forwarder that consumes each registered entry is
//! wired in the Phase 8c router unit (U10).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::SystemTime;

use parking_lot::Mutex;

use super::protocol::ShimProtocol;

/// Monotonic per-registry connection id.
pub type ShimConnId = u64;

/// One entry per registered shim connection.
#[derive(Debug, Clone)]
pub struct ShimConnEntry {
    pub protocol: ShimProtocol,
    pub pid: u32,
    pub connected_at: SystemTime,
}

/// Shim registry. Phase 8c (U3) exposes this as part of the daemon's
/// public API; the byte-pump forwarder that consumes each registered
/// entry lands in U10.
#[derive(Debug, Default)]
pub struct ShimRegistry {
    inner: Mutex<HashMap<ShimConnId, ShimConnEntry>>,
    next_id: AtomicU64,
}

impl ShimRegistry {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Insert a new entry and return a RAII [`ShimHandle`] that
    /// removes the entry when dropped.
    pub fn register(self: &Arc<Self>, protocol: ShimProtocol, pid: u32) -> ShimHandle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = ShimConnEntry {
            protocol,
            pid,
            connected_at: SystemTime::now(),
        };
        self.inner.lock().insert(id, entry);
        ShimHandle {
            registry: Arc::downgrade(self),
            id,
        }
    }

    /// Count of currently registered entries. Task 9's bootstrap path
    /// surfaces this via `daemon/status`.
    ///
    /// The returned value is a snapshot taken under a
    /// `parking_lot::Mutex`; callers must not hold the returned
    /// `usize` "logically live" across long-running awaits that could
    /// re-enter the registry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Returns `true` when no shim connections are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }

    /// Atomic bounded admission: reads the current entry count and
    /// inserts a new entry under a SINGLE `parking_lot::Mutex` guard.
    /// Guarantees `len() <= cap` at all times (subject to the
    /// `Ordering::Relaxed` monotonic `next_id` counter, which is not
    /// part of the cap check).
    ///
    /// Returns `Err(RejectReason::CapExceeded { current, cap })` if
    /// admission would exceed the cap. On accept, returns a RAII
    /// [`ShimHandle`] whose `Drop` removes the entry from the registry.
    ///
    /// Replaces the Phase 8c-era `len() >= cap` + [`Self::register`]
    /// two-step which was racy under concurrent registrations (Codex
    /// iter-0 B2 blocker): two threads could both observe
    /// `len == cap - 1` and both insert, oversubscribing the cap.
    ///
    /// # Errors
    ///
    /// Returns [`RejectReason::CapExceeded`] when the registry already
    /// holds `cap` or more entries.
    pub fn try_register_bounded(
        self: &Arc<Self>,
        protocol: ShimProtocol,
        pid: u32,
        cap: usize,
    ) -> Result<ShimHandle, RejectReason> {
        let mut guard = self.inner.lock();
        let current = guard.len();
        if current >= cap {
            return Err(RejectReason::CapExceeded { current, cap });
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = ShimConnEntry {
            protocol,
            pid,
            connected_at: SystemTime::now(),
        };
        guard.insert(id, entry);
        drop(guard);
        Ok(ShimHandle {
            registry: Arc::downgrade(self),
            id,
        })
    }
}

/// Why a [`ShimRegistry::try_register_bounded`] call was rejected.
///
/// Returned when admission fails under the configured cap. Matches the
/// [`sqry_daemon_protocol::ShimRegisterAck`] wire form: the router
/// copies [`Display`](std::fmt::Display)'s output into
/// `ack.reason` on reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectReason {
    /// Registry already holds `current` entries; `cap` cannot be
    /// exceeded. The router reports this as
    /// `"shim registry full ({current} / {cap})"` on the wire.
    CapExceeded {
        /// Current number of registered entries observed under the
        /// admission mutex at reject time.
        current: usize,
        /// Configured cap that admission would have exceeded.
        cap: usize,
    },
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapExceeded { current, cap } => {
                write!(f, "shim registry full ({current} / {cap})")
            }
        }
    }
}

impl std::error::Error for RejectReason {}

/// RAII handle removing the associated [`ShimRegistry`] entry on drop.
#[derive(Debug)]
pub struct ShimHandle {
    registry: Weak<ShimRegistry>,
    id: ShimConnId,
}

impl Drop for ShimHandle {
    fn drop(&mut self) {
        if let Some(reg) = self.registry.upgrade() {
            reg.inner.lock().remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_drop_cleans_up() {
        let reg = ShimRegistry::new();
        assert_eq!(reg.len(), 0);
        let handle = reg.register(ShimProtocol::Lsp, 42);
        assert_eq!(reg.len(), 1);
        drop(handle);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn multiple_registrations_unique_ids() {
        let reg = ShimRegistry::new();
        let h1 = reg.register(ShimProtocol::Lsp, 1);
        let h2 = reg.register(ShimProtocol::Mcp, 2);
        assert_ne!(h1.id, h2.id);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn try_register_bounded_rejects_when_at_cap() {
        let reg = ShimRegistry::new();
        // Fill to cap 2 using bounded admission.
        let h1 = reg.try_register_bounded(ShimProtocol::Lsp, 1, 2).unwrap();
        let h2 = reg.try_register_bounded(ShimProtocol::Mcp, 2, 2).unwrap();
        assert_eq!(reg.len(), 2);
        // Third attempt must be rejected.
        let err = reg
            .try_register_bounded(ShimProtocol::Lsp, 3, 2)
            .expect_err("must reject at cap");
        match err {
            RejectReason::CapExceeded { current, cap } => {
                assert_eq!(current, 2);
                assert_eq!(cap, 2);
            }
        }
        // len unchanged after rejection.
        assert_eq!(reg.len(), 2);
        // Drop a handle → room frees up → next admission succeeds.
        drop(h1);
        let h3 = reg.try_register_bounded(ShimProtocol::Lsp, 3, 2).unwrap();
        assert_eq!(reg.len(), 2);
        drop(h2);
        drop(h3);
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn reject_reason_display_matches_wire_form() {
        let r = RejectReason::CapExceeded {
            current: 256,
            cap: 256,
        };
        assert_eq!(r.to_string(), "shim registry full (256 / 256)");
    }

    #[test]
    fn try_register_bounded_cap_race_256_concurrent_admits_exactly_cap() {
        // Thread + Barrier harness per Codex iter-1 Q6 recommendation.
        // Spawn 300 threads, all block on the Barrier, then race into
        // try_register_bounded(cap=256). Count successes — must be
        // exactly 256. This is the iter-0 B2 blocker proof: a racy
        // `len() >= cap` + `register()` two-step would oversubscribe
        // under this load; the single-mutex-scope implementation must
        // not.
        use std::sync::Barrier;
        use std::thread;

        let reg = ShimRegistry::new();
        let cap: usize = 256;
        let n_threads: usize = 300;
        let barrier = Arc::new(Barrier::new(n_threads));

        let handles: Vec<_> = (0..n_threads)
            .map(|i| {
                let reg = Arc::clone(&reg);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || -> Option<ShimHandle> {
                    barrier.wait();
                    reg.try_register_bounded(ShimProtocol::Lsp, i as u32, cap)
                        .ok()
                })
            })
            .collect();

        let mut ok_handles = Vec::new();
        let mut rejected = 0usize;
        for h in handles {
            match h.join().unwrap() {
                Some(handle) => ok_handles.push(handle),
                None => rejected += 1,
            }
        }
        assert_eq!(ok_handles.len(), cap, "exactly cap admissions must succeed");
        assert_eq!(rejected, n_threads - cap, "remaining must be rejected");
        assert_eq!(reg.len(), cap);

        // Drop all handles; registry should be empty.
        drop(ok_handles);
        assert_eq!(reg.len(), 0);
    }
}
