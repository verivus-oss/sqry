//! Uses collector - fire-and-forget event capture
//!
//! This module provides the main entry point for recording use events.
//! Events are captured via a bounded channel and written to disk by a
//! background thread, ensuring zero blocking on the main execution path.
//!
//! # Design
//!
//! - **Bounded channel** (1000 events) prevents unbounded memory growth
//! - **`try_send` semantics** - drops events when saturated, never blocks
//! - **Dropped event counter** - tracked for diagnostics visibility
//! - **Fire-and-forget** - `record()` returns immediately
//! - **Background thread** - handles disk I/O asynchronously
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_core::uses::{UsesCollector, UseEvent, UseEventType, QueryKind};
//!
//! // Create collector (typically done once at startup)
//! let collector = UsesCollector::new(&uses_dir, true)?;
//!
//! // Record events (non-blocking)
//! collector.record(UseEvent::new(UseEventType::QueryExecuted {
//!     kind: QueryKind::CallChain,
//!     result_count: 42,
//! }));
//!
//! // Use RAII timer for automatic duration capture
//! {
//!     let _timer = collector.timed(UseEventType::QueryExecuted {
//!         kind: QueryKind::SymbolLookup,
//!         result_count: 0,  // Will be updated
//!     });
//!     // ... do work ...
//! } // Event recorded on drop with duration
//! ```

use super::storage::UsesWriter;
use super::types::{UseEvent, UseEventType};
use chrono::Utc;
use crossbeam_channel::{Sender, TrySendError, bounded};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Capacity of the bounded event channel
///
/// This limits memory usage while providing headroom for bursts.
/// If the queue fills up, events are dropped (tracked in `dropped_events`).
const CHANNEL_CAPACITY: usize = 1000;

/// Global uses collector - fire-and-forget event capture
///
/// Uses a bounded channel to prevent unbounded memory growth.
/// Events are written to disk by a background thread.
#[derive(Clone)]
pub struct UsesCollector {
    sender: Sender<UseEvent>,
    enabled: Arc<AtomicBool>,
    dropped_events: Arc<AtomicU64>,
}

impl UsesCollector {
    /// Create a new uses collector
    ///
    /// Spawns a background thread that writes events to disk.
    ///
    /// # Arguments
    ///
    /// * `uses_dir` - Directory to store event logs (e.g., `~/.sqry/uses/`)
    /// * `enabled` - Whether to actually record events
    ///
    /// # Returns
    ///
    /// A new `UsesCollector` instance. The background writer thread is
    /// automatically spawned and will run until the collector is dropped.
    #[must_use]
    pub fn new(uses_dir: &Path, enabled: bool) -> Self {
        let (sender, receiver) = bounded(CHANNEL_CAPACITY);

        // Spawn background writer thread
        let writer = UsesWriter::new(uses_dir.to_path_buf());
        std::thread::spawn(move || writer.run(&receiver));

        Self {
            sender,
            enabled: Arc::new(AtomicBool::new(enabled)),
            dropped_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a collector for testing that discards events
    ///
    /// The test collector still tracks dropped events but doesn't write to disk.
    #[cfg(test)]
    #[must_use]
    pub fn new_test() -> Self {
        let (sender, receiver) = bounded(CHANNEL_CAPACITY);

        // Spawn a thread that just drains the receiver
        std::thread::spawn(move || {
            while receiver.recv().is_ok() {
                // Discard events
            }
        });

        Self {
            sender,
            enabled: Arc::new(AtomicBool::new(true)),
            dropped_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a disabled collector that doesn't record anything
    #[must_use]
    pub fn disabled() -> Self {
        let (sender, _receiver) = bounded(1);

        Self {
            sender,
            enabled: Arc::new(AtomicBool::new(false)),
            dropped_events: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Record a use event (non-blocking, fire-and-forget)
    ///
    /// Uses `try_send` - drops events if queue is full (backpressure).
    /// Disconnected errors are also ignored for graceful shutdown.
    ///
    /// # Arguments
    ///
    /// * `event` - The event to record
    pub fn record(&self, event: UseEvent) {
        if self.enabled.load(Ordering::Relaxed)
            && let Err(TrySendError::Full(_)) = self.sender.try_send(event)
        {
            self.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
        // Disconnected errors also ignored - graceful shutdown
    }

    /// Record an event of a specific type with the current timestamp
    ///
    /// Convenience method that creates a `UseEvent` and records it.
    pub fn record_event(&self, event_type: UseEventType) {
        self.record(UseEvent::new(event_type));
    }

    /// Create a scoped timer that auto-records duration on drop
    ///
    /// Returns a `CollectorTimedUse` that, when dropped, records the event
    /// with the elapsed duration.
    ///
    /// # Arguments
    ///
    /// * `event_type` - The event type to record (duration will be added)
    ///
    /// # Returns
    ///
    /// A timer that records the event on drop
    #[must_use]
    pub fn timed(&self, event_type: UseEventType) -> CollectorTimedUse<'_> {
        CollectorTimedUse::new(self, event_type)
    }

    /// Enable or disable event recording
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Check if recording is enabled
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Get count of dropped events (for diagnostics)
    ///
    /// Events are dropped when the queue is full (backpressure).
    /// A high count may indicate performance issues.
    #[must_use]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_events.load(Ordering::Relaxed)
    }

    /// Reset the dropped events counter
    pub fn reset_dropped_count(&self) {
        self.dropped_events.store(0, Ordering::Relaxed);
    }
}

/// RAII timer for collector - records event with duration on drop
///
/// Created via `UsesCollector::timed()`. When dropped, records the event
/// with the elapsed duration since creation.
pub struct CollectorTimedUse<'a> {
    collector: &'a UsesCollector,
    event_type: Option<UseEventType>,
    start: Instant,
}

impl<'a> CollectorTimedUse<'a> {
    /// Create a new timer for the given collector and event type
    fn new(collector: &'a UsesCollector, event_type: UseEventType) -> Self {
        Self {
            collector,
            event_type: Some(event_type),
            start: Instant::now(),
        }
    }

    /// Cancel the timer without recording an event
    ///
    /// Use this when an operation fails and you don't want to record it.
    pub fn cancel(&mut self) {
        self.event_type = None;
    }

    /// Complete the timer with an updated event type
    ///
    /// Use this when you need to update the event type after the operation
    /// completes (e.g., to update result count).
    pub fn complete_with(mut self, event_type: UseEventType) {
        self.event_type = Some(event_type);
        // Drop will record the event
    }
}

impl Drop for CollectorTimedUse<'_> {
    fn drop(&mut self) {
        if let Some(event_type) = self.event_type.take() {
            let duration_ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
            let event = UseEvent {
                timestamp: Utc::now(),
                event_type,
                duration_ms: Some(duration_ms),
            };
            self.collector.record(event);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uses::types::QueryKind;
    use std::time::Duration;

    #[test]
    fn test_collector_fire_and_forget() {
        let collector = UsesCollector::new_test();
        let start = Instant::now();

        for _ in 0..1000 {
            collector.record(UseEvent::new(UseEventType::QueryExecuted {
                kind: QueryKind::CallChain,
                result_count: 42,
            }));
        }

        let elapsed = start.elapsed();
        // 1000 events should complete in well under 100ms
        assert!(
            elapsed < Duration::from_millis(100),
            "Recording 1000 events took {elapsed:?}"
        );
    }

    #[test]
    fn test_collector_disabled() {
        let collector = UsesCollector::disabled();

        // Should not panic or block
        collector.record(UseEvent::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        }));

        assert!(!collector.is_enabled());
    }

    #[test]
    fn test_collector_respects_disabled() {
        let collector = UsesCollector::new_test();
        collector.set_enabled(false);

        // Should not record when disabled
        collector.record(UseEvent::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        }));

        // Can re-enable
        collector.set_enabled(true);
        assert!(collector.is_enabled());
    }

    #[test]
    fn test_timed_use_records_duration() {
        let collector = UsesCollector::new_test();

        {
            let _timer = collector.timed(UseEventType::QueryExecuted {
                kind: QueryKind::SymbolLookup,
                result_count: 0,
            });
            std::thread::sleep(Duration::from_millis(10));
        }

        // Event should have been recorded (we can't easily check the duration
        // without more infrastructure, but we verify no panics)
    }

    #[test]
    fn test_timed_use_cancel() {
        let collector = UsesCollector::new_test();

        {
            let mut timer = collector.timed(UseEventType::QueryExecuted {
                kind: QueryKind::SymbolLookup,
                result_count: 0,
            });
            timer.cancel();
        }

        // No event should be recorded (verified by no panics)
    }

    #[test]
    fn test_dropped_count() {
        let collector = UsesCollector::disabled();

        assert_eq!(collector.dropped_count(), 0);
        collector.reset_dropped_count();
        assert_eq!(collector.dropped_count(), 0);
    }

    #[test]
    fn test_record_event_convenience() {
        let collector = UsesCollector::new_test();

        collector.record_event(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        });

        // Should not panic
    }
}
