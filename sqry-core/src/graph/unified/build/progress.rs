//! Progress reporting for graph build phases.
//!
//! This module provides utilities for reporting progress during the 4-pass
//! graph build pipeline with thread-safe aggregation and time-based throttling.
//!
//! # Thread Safety
//!
//! Graph build passes may run in parallel. This module uses atomic counters
//! for thread-safe progress aggregation and time-based throttling to avoid
//! overwhelming the progress reporter (max 60 updates/second per NFR-2).
//!
//! # Panic Safety
//!
//! Reporter calls are wrapped in `catch_unwind` to ensure that progress
//! reporting failures never abort the build. Panics are logged and ignored.
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::build::progress::GraphBuildProgressTracker;
//! use sqry_core::progress::no_op_reporter;
//!
//! let tracker = GraphBuildProgressTracker::new(no_op_reporter());
//!
//! // Start phase 1
//! tracker.start_phase(1, "AST extraction", 1000);
//!
//! // Report progress (throttled)
//! for i in 0..1000 {
//!     tracker.increment_progress();
//! }
//!
//! // Complete phase
//! tracker.complete_phase();
//! ```

use std::panic;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::progress::{IndexProgress, SharedReporter};

/// Safely report a progress event, catching any panics.
///
/// Progress reporting should never abort a build. If the reporter panics,
/// the error is logged and the build continues.
fn safe_report(reporter: &SharedReporter, event: IndexProgress) {
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        reporter.report(event);
    }));

    if let Err(e) = result {
        // Log the panic but don't propagate it
        let msg = if let Some(s) = e.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = e.downcast_ref::<String>() {
            s.clone()
        } else {
            "unknown panic".to_string()
        };
        log::warn!("Progress reporter panicked (ignored): {msg}");
    }
}

/// Minimum interval between progress updates (16.67ms = 60 Hz max)
const MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(17);

/// Progress tracker for graph build phases with thread-safe aggregation.
///
/// Tracks progress within a single phase and provides time-based throttling
/// to limit update frequency to 60 Hz or less.
pub struct GraphBuildProgressTracker {
    /// Shared progress reporter
    reporter: SharedReporter,

    /// Current phase state (protected by mutex for phase transitions)
    phase_state: Mutex<PhaseState>,

    /// Atomic counter for thread-safe progress updates
    items_processed: AtomicUsize,

    /// Total items in current phase (set at phase start)
    total_items: AtomicUsize,
}

/// Internal state for the current phase.
struct PhaseState {
    /// Current phase number (1-4), or 0 if no phase active
    phase_number: u8,

    /// Phase name for display
    phase_name: &'static str,

    /// When the current phase started
    phase_start: Instant,

    /// When we last emitted a progress update
    last_update: Instant,
}

impl Default for PhaseState {
    fn default() -> Self {
        Self {
            phase_number: 0,
            phase_name: "",
            phase_start: Instant::now(),
            last_update: Instant::now(),
        }
    }
}

impl GraphBuildProgressTracker {
    /// Create a new progress tracker with the given reporter.
    #[must_use]
    pub fn new(reporter: SharedReporter) -> Self {
        Self {
            reporter,
            phase_state: Mutex::new(PhaseState::default()),
            items_processed: AtomicUsize::new(0),
            total_items: AtomicUsize::new(0),
        }
    }

    /// Start a new graph build phase.
    ///
    /// # Arguments
    ///
    /// * `phase_number` - Phase number (1-4)
    /// * `phase_name` - Human-readable phase name
    /// * `total_items` - Total items to process in this phase
    ///
    /// # Panics
    ///
    /// Panics if the phase state mutex is poisoned.
    pub fn start_phase(&self, phase_number: u8, phase_name: &'static str, total_items: usize) {
        // Reset counters
        self.items_processed.store(0, Ordering::SeqCst);
        self.total_items.store(total_items, Ordering::SeqCst);

        // Update phase state
        {
            let mut state = self.phase_state.lock().unwrap();
            state.phase_number = phase_number;
            state.phase_name = phase_name;
            state.phase_start = Instant::now();
            state.last_update = Instant::now();
        }

        // Report phase start (panic-safe)
        safe_report(
            &self.reporter,
            IndexProgress::GraphPhaseStarted {
                phase_number,
                phase_name,
                total_items,
            },
        );
    }

    /// Increment progress counter by one (thread-safe).
    ///
    /// Emits a progress update if enough time has passed since the last update
    /// (time-based throttling at 60 Hz max).
    pub fn increment_progress(&self) {
        self.add_progress(1);
    }

    /// Add to progress counter (thread-safe).
    ///
    /// Emits a progress update if enough time has passed since the last update.
    pub fn add_progress(&self, count: usize) {
        let new_count = self.items_processed.fetch_add(count, Ordering::SeqCst) + count;
        self.maybe_emit_progress(new_count);
    }

    /// Check if we should emit a progress update (time-based throttling).
    fn maybe_emit_progress(&self, items_processed: usize) {
        let total = self.total_items.load(Ordering::SeqCst);

        // Try to acquire lock without blocking (non-contended fast path)
        // Capture phase_number while holding the lock to avoid drift
        let emit_info = {
            let Ok(mut state) = self.phase_state.try_lock() else {
                // Another thread is updating, skip this update
                return;
            };

            let now = Instant::now();
            if now.duration_since(state.last_update) >= MIN_UPDATE_INTERVAL {
                state.last_update = now;
                Some(state.phase_number)
            } else {
                None
            }
        };

        if let Some(phase_number) = emit_info {
            safe_report(
                &self.reporter,
                IndexProgress::GraphPhaseProgress {
                    phase_number,
                    items_processed,
                    total_items: total,
                },
            );
        }
    }

    /// Complete the current phase and report duration.
    ///
    /// # Panics
    ///
    /// Panics if the phase state mutex is poisoned.
    pub fn complete_phase(&self) {
        let (phase_number, phase_name, phase_duration) = {
            let state = self.phase_state.lock().unwrap();
            (
                state.phase_number,
                state.phase_name,
                state.phase_start.elapsed(),
            )
        };

        safe_report(
            &self.reporter,
            IndexProgress::GraphPhaseCompleted {
                phase_number,
                phase_name,
                phase_duration,
            },
        );
    }

    /// Report that index saving has started for a component.
    pub fn start_saving(&self, component_name: &'static str) {
        safe_report(
            &self.reporter,
            IndexProgress::SavingStarted { component_name },
        );
    }

    /// Report that index saving has completed for a component.
    pub fn complete_saving(&self, component_name: &'static str, save_duration: Duration) {
        safe_report(
            &self.reporter,
            IndexProgress::SavingCompleted {
                component_name,
                save_duration,
            },
        );
    }

    /// Get the current progress count (for testing).
    #[cfg(test)]
    pub fn current_progress(&self) -> usize {
        self.items_processed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::no_op_reporter;
    use std::sync::Arc;

    struct EventCapture {
        events: Mutex<Vec<IndexProgress>>,
    }

    impl EventCapture {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
            })
        }

        fn events(&self) -> Vec<IndexProgress> {
            self.events.lock().unwrap().clone()
        }

        fn event_count(&self) -> usize {
            self.events.lock().unwrap().len()
        }
    }

    impl crate::progress::ProgressReporter for EventCapture {
        fn report(&self, event: IndexProgress) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn test_phase_lifecycle() {
        let capture = EventCapture::new();
        let tracker = GraphBuildProgressTracker::new(capture.clone());

        tracker.start_phase(1, "Test phase", 100);
        tracker.complete_phase();

        let events = capture.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            IndexProgress::GraphPhaseStarted {
                phase_number: 1,
                phase_name: "Test phase",
                total_items: 100
            }
        ));
        assert!(matches!(
            events[1],
            IndexProgress::GraphPhaseCompleted {
                phase_number: 1,
                phase_name: "Test phase",
                ..
            }
        ));
    }

    #[test]
    fn test_progress_increment() {
        let capture = EventCapture::new();
        let tracker = GraphBuildProgressTracker::new(capture.clone());

        tracker.start_phase(2, "Increment test", 10);

        // First increment should emit (enough time has passed)
        tracker.increment_progress();
        assert_eq!(tracker.current_progress(), 1);

        tracker.complete_phase();

        // Should have at least start + complete events
        assert!(capture.event_count() >= 2);
    }

    #[test]
    fn test_saving_events() {
        let capture = EventCapture::new();
        let tracker = GraphBuildProgressTracker::new(capture.clone());

        tracker.start_saving("symbols");
        tracker.complete_saving("symbols", Duration::from_millis(100));

        let events = capture.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            IndexProgress::SavingStarted {
                component_name: "symbols"
            }
        ));
        assert!(matches!(
            events[1],
            IndexProgress::SavingCompleted {
                component_name: "symbols",
                ..
            }
        ));
    }

    #[test]
    fn test_no_op_reporter_no_panic() {
        let tracker = GraphBuildProgressTracker::new(no_op_reporter());

        tracker.start_phase(1, "No-op test", 1000);
        for _ in 0..1000 {
            tracker.increment_progress();
        }
        tracker.complete_phase();
        // Should complete without panic
    }

    #[test]
    fn test_throttling_limits_updates() {
        let capture = EventCapture::new();
        let tracker = GraphBuildProgressTracker::new(capture.clone());

        tracker.start_phase(3, "Throttle test", 10000);

        // Rapid updates should be throttled
        for _ in 0..1000 {
            tracker.increment_progress();
        }

        tracker.complete_phase();

        // With 17ms throttle interval, 1000 rapid updates should result in
        // far fewer than 1000 progress events (likely just start + 1-2 + complete)
        let progress_events = capture
            .events()
            .iter()
            .filter(|e| matches!(e, IndexProgress::GraphPhaseProgress { .. }))
            .count();

        // Should have significantly fewer progress events than increments
        assert!(
            progress_events < 100,
            "Expected throttling to limit updates"
        );
    }

    /// A reporter that panics on every report call.
    struct PanickingReporter;

    impl crate::progress::ProgressReporter for PanickingReporter {
        fn report(&self, _event: IndexProgress) {
            panic!("Intentional test panic from PanickingReporter");
        }
    }

    #[test]
    fn test_safe_report_catches_panics() {
        // Create a reporter that panics
        let reporter: SharedReporter = Arc::new(PanickingReporter);

        // safe_report should catch the panic and not propagate it
        // This should not panic the test
        safe_report(
            &reporter,
            IndexProgress::SavingStarted {
                component_name: "test",
            },
        );

        // If we got here, the panic was successfully caught
    }

    #[test]
    fn test_tracker_with_panicking_reporter_continues() {
        // Create tracker with a panicking reporter
        let tracker = GraphBuildProgressTracker::new(Arc::new(PanickingReporter));

        // All operations should complete without propagating the panic
        tracker.start_phase(1, "Panic test", 100);
        tracker.increment_progress();
        tracker.add_progress(5);
        tracker.complete_phase();
        tracker.start_saving("test");
        tracker.complete_saving("test", Duration::from_millis(10));

        // If we got here, all panics were caught and the build continued
        assert_eq!(tracker.current_progress(), 6);
    }
}
