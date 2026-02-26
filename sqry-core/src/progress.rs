//! Progress reporting for indexing operations.
//!
//! This module provides types for tracking and reporting progress during
//! graph indexing operations. Progress events can be used to implement
//! progress bars, logging, or other user feedback mechanisms.
//!
//! # Example
//!
//! ```rust
//! use sqry_core::progress::{IndexProgress, ProgressReporter};
//! use std::sync::Arc;
//!
//! struct ConsoleProgress;
//!
//! impl ProgressReporter for ConsoleProgress {
//!     fn report(&self, event: IndexProgress) {
//!         match event {
//!             IndexProgress::Started { total_files } => {
//!                 println!("Starting to index {} files...", total_files);
//!             }
//!             IndexProgress::FileCompleted { path, symbols } => {
//!                 println!("Processed {:?}: {} items", path, symbols);
//!             }
//!             IndexProgress::Completed { total_symbols, duration } => {
//!                 println!("Indexed {} items in {:?}", total_symbols, duration);
//!             }
//!             _ => {}
//!         }
//!     }
//! }
//!
//! // Use ConsoleProgress when building graphs with progress reporting
//! let reporter: Arc<dyn ProgressReporter> = Arc::new(ConsoleProgress);
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::graph::NodeKind;

/// Progress events emitted during indexing operations.
///
/// # Stability
///
/// This enum is marked `#[non_exhaustive]` to allow adding new progress events
/// in future versions without breaking downstream code. Always include a
/// wildcard arm (`_ => {}`) when matching on this enum.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IndexProgress {
    // === File Processing Events ===
    /// Indexing has started.
    Started {
        /// Total number of files to process
        total_files: usize,
    },

    /// A file is currently being processed.
    FileProcessing {
        /// Path to the file being processed
        path: PathBuf,
        /// Current file number (1-based)
        current: usize,
        /// Total number of files
        total: usize,
    },

    /// A file has been processed successfully.
    FileCompleted {
        /// Path to the completed file
        path: PathBuf,
        /// Number of items extracted from this file
        symbols: usize,
    },

    // === Ingest Progress ===
    /// Progress while ingesting nodes and relations into the index.
    IngestProgress {
        /// Files ingested so far
        files_processed: usize,
        /// Total files to ingest
        total_files: usize,
        /// Total items ingested so far
        total_symbols: usize,
        /// Node-kind breakdown
        counts: NodeIngestCounts,
        /// Elapsed time for ingestion
        elapsed: Duration,
        /// Estimated remaining time (best-effort)
        eta: Option<Duration>,
    },

    /// A file is about to be ingested into the index.
    IngestFileStarted {
        /// Path to the file being ingested
        path: PathBuf,
        /// Current file number (1-based)
        current: usize,
        /// Total number of files to ingest
        total: usize,
    },

    /// A file has finished ingesting into the index.
    IngestFileCompleted {
        /// Path to the ingested file
        path: PathBuf,
        /// Number of items ingested from this file
        symbols: usize,
        /// Duration of the ingest work for this file
        duration: Duration,
    },

    // === Stage Events ===
    /// A coarse-grained indexing stage has started.
    StageStarted {
        /// Human-readable stage name (e.g., "Resolve imports")
        /// Uses `&'static str` to avoid allocations.
        stage_name: &'static str,
    },

    /// A coarse-grained indexing stage has completed.
    StageCompleted {
        /// Human-readable stage name
        stage_name: &'static str,
        /// Duration of the stage
        stage_duration: Duration,
    },

    // === Graph Building Events ===
    /// A graph build phase has started.
    GraphPhaseStarted {
        /// Phase number (1-4)
        phase_number: u8,
        /// Human-readable phase name (e.g., "AST extraction")
        /// Uses `&'static str` to avoid allocations in hot paths.
        phase_name: &'static str,
        /// Total items to process in this phase
        total_items: usize,
    },

    /// Progress within a graph build phase.
    GraphPhaseProgress {
        /// Phase number (1-4)
        phase_number: u8,
        /// Number of items processed so far
        items_processed: usize,
        /// Total items in this phase
        total_items: usize,
    },

    /// A graph build phase has completed.
    GraphPhaseCompleted {
        /// Phase number (1-4)
        phase_number: u8,
        /// Human-readable phase name
        phase_name: &'static str,
        /// Duration of this phase
        phase_duration: Duration,
    },

    // === Index Saving Events ===
    /// Index save operation has started for a component.
    SavingStarted {
        /// Component being saved (e.g., "symbols", "trigrams", "unified graph")
        /// Uses `&'static str` to avoid allocations.
        component_name: &'static str,
    },

    /// Index save operation has completed for a component.
    SavingCompleted {
        /// Component that was saved
        component_name: &'static str,
        /// Duration of the save operation
        save_duration: Duration,
    },

    // === Completion Event ===
    /// Indexing has completed.
    Completed {
        /// Total number of items indexed
        total_symbols: usize,
        /// Duration of the indexing operation
        duration: Duration,
    },
}

/// Trait for reporting progress during indexing operations.
///
/// Implementors can display progress bars, log events, or perform
/// other actions in response to indexing progress.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support parallel indexing.
/// Progress events may be reported from multiple threads concurrently.
pub trait ProgressReporter: Send + Sync {
    /// Report a progress event.
    ///
    /// This method is called during indexing to report progress.
    /// Implementations should be non-blocking to avoid slowing down
    /// the indexing process.
    fn report(&self, event: IndexProgress);
}

/// Helper for emitting coarse-grained stage progress.
pub struct ProgressStage {
    reporter: SharedReporter,
    stage_name: &'static str,
    start: Instant,
}

impl ProgressStage {
    /// Emit a stage start event and return a timer for completion.
    #[must_use]
    pub fn start(reporter: &SharedReporter, stage_name: &'static str) -> Self {
        reporter.report(IndexProgress::StageStarted { stage_name });
        Self {
            reporter: Arc::clone(reporter),
            stage_name,
            start: Instant::now(),
        }
    }

    /// Emit a stage completion event.
    pub fn finish(self) {
        self.reporter.report(IndexProgress::StageCompleted {
            stage_name: self.stage_name,
            stage_duration: self.start.elapsed(),
        });
    }
}

/// Node-kind counters for ingestion progress reporting.
#[derive(Debug, Clone, Default)]
pub struct NodeIngestCounts {
    /// Function nodes.
    pub functions: usize,
    /// Class nodes.
    pub classes: usize,
    /// Method nodes.
    pub methods: usize,
    /// Struct nodes.
    pub structs: usize,
    /// Enum nodes.
    pub enums: usize,
    /// Interface/trait nodes.
    pub interfaces: usize,
    /// Variable-like nodes (variables, properties, parameters).
    pub variables: usize,
    /// Constant nodes.
    pub constants: usize,
    /// Type alias nodes.
    pub types: usize,
    /// Module nodes.
    pub modules: usize,
    /// All other nodes not covered by the explicit buckets.
    pub other: usize,
}

impl NodeIngestCounts {
    /// Add a single node kind to the appropriate counter.
    pub fn add_node_kind(&mut self, kind: &NodeKind) {
        match kind {
            NodeKind::Function { .. } => self.functions += 1,
            NodeKind::Class { .. } => self.classes += 1,
            NodeKind::Module { .. } => self.modules += 1,
            NodeKind::Variable { .. } => self.variables += 1,
        }
    }

    /// Add a slice of node kinds to the counters.
    pub fn add_node_kinds(&mut self, kinds: &[NodeKind]) {
        for kind in kinds {
            self.add_node_kind(kind);
        }
    }

    /// Total number of nodes across all buckets.
    #[must_use]
    pub fn total(&self) -> usize {
        self.functions
            + self.classes
            + self.methods
            + self.structs
            + self.enums
            + self.interfaces
            + self.variables
            + self.constants
            + self.types
            + self.modules
            + self.other
    }
}

/// Time-throttled ingestion progress tracker.
pub struct IngestProgressTracker {
    reporter: SharedReporter,
    total_files: usize,
    processed_files: usize,
    counts: NodeIngestCounts,
    start: Instant,
    last_emit: Instant,
}

impl IngestProgressTracker {
    /// Create a new ingestion progress tracker.
    #[must_use]
    pub fn new(reporter: &SharedReporter, total_files: usize) -> Self {
        let now = Instant::now();
        Self {
            reporter: Arc::clone(reporter),
            total_files,
            processed_files: 0,
            counts: NodeIngestCounts::default(),
            start: now,
            last_emit: now,
        }
    }

    /// Record the node kinds ingested for one file and emit a progress update if needed.
    pub fn record_node_kinds(&mut self, kinds: &[NodeKind]) {
        self.processed_files = self.processed_files.saturating_add(1);
        self.counts.add_node_kinds(kinds);
        self.maybe_emit(false);
    }

    /// Emit a final progress update.
    pub fn finish(&mut self) {
        self.maybe_emit(true);
    }

    fn maybe_emit(&mut self, force: bool) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.start);
        if !force && now.duration_since(self.last_emit) < Duration::from_millis(800) {
            return;
        }
        self.last_emit = now;

        let eta = self.estimate_eta(elapsed);
        self.reporter.report(IndexProgress::IngestProgress {
            files_processed: self.processed_files,
            total_files: self.total_files,
            total_symbols: self.counts.total(),
            counts: self.counts.clone(),
            elapsed,
            eta,
        });
    }

    fn estimate_eta(&self, elapsed: Duration) -> Option<Duration> {
        if self.processed_files == 0 || self.total_files == 0 {
            return None;
        }
        let elapsed_nanos = elapsed.as_nanos();
        if elapsed_nanos == 0 {
            return None;
        }
        let processed_files = u128::from(self.processed_files as u64);
        let remaining_files =
            u128::from(self.total_files.saturating_sub(self.processed_files) as u64);
        if processed_files == 0 || remaining_files == 0 {
            return Some(Duration::from_secs(0));
        }
        let nanos_per_file = elapsed_nanos / processed_files;
        let remaining_nanos = nanos_per_file.saturating_mul(remaining_files);
        let remaining_nanos_u64 = u64::try_from(remaining_nanos).ok()?;
        Some(Duration::from_nanos(remaining_nanos_u64))
    }
}

/// A no-op progress reporter that discards all events.
///
/// This is the default reporter used when no progress reporting is needed.
#[derive(Debug, Clone, Copy)]
pub struct NoOpReporter;

impl ProgressReporter for NoOpReporter {
    fn report(&self, _event: IndexProgress) {
        // Intentionally empty - no progress reporting
    }
}

/// Type alias for a shared progress reporter.
pub type SharedReporter = Arc<dyn ProgressReporter>;

/// Creates a new no-op reporter.
///
/// This is useful as a default when no progress reporting is needed.
#[must_use]
pub fn no_op_reporter() -> SharedReporter {
    Arc::new(NoOpReporter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct TestReporter {
        events: Mutex<Vec<IndexProgress>>,
    }

    impl TestReporter {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<IndexProgress> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ProgressReporter for TestReporter {
        fn report(&self, event: IndexProgress) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn test_progress_event_sequence() {
        let reporter = TestReporter::new();

        // Simulate indexing progress
        reporter.report(IndexProgress::Started { total_files: 2 });
        reporter.report(IndexProgress::FileProcessing {
            path: PathBuf::from("file1.rs"),
            current: 1,
            total: 2,
        });
        reporter.report(IndexProgress::FileCompleted {
            path: PathBuf::from("file1.rs"),
            symbols: 10,
        });
        reporter.report(IndexProgress::FileProcessing {
            path: PathBuf::from("file2.rs"),
            current: 2,
            total: 2,
        });
        reporter.report(IndexProgress::FileCompleted {
            path: PathBuf::from("file2.rs"),
            symbols: 15,
        });
        reporter.report(IndexProgress::Completed {
            total_symbols: 25,
            duration: Duration::from_secs(1),
        });

        let events = reporter.events();
        assert_eq!(events.len(), 6);

        // Verify event types in order
        matches!(events[0], IndexProgress::Started { .. });
        matches!(events[1], IndexProgress::FileProcessing { .. });
        matches!(events[2], IndexProgress::FileCompleted { .. });
        matches!(events[3], IndexProgress::FileProcessing { .. });
        matches!(events[4], IndexProgress::FileCompleted { .. });
        matches!(events[5], IndexProgress::Completed { .. });
    }

    #[test]
    fn test_no_op_reporter() {
        let reporter = no_op_reporter();

        // Should not panic or produce side effects
        reporter.report(IndexProgress::Started { total_files: 5 });
        reporter.report(IndexProgress::Completed {
            total_symbols: 100,
            duration: Duration::from_millis(500),
        });
    }

    #[test]
    fn test_reporter_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoOpReporter>();
        assert_send_sync::<TestReporter>();
    }
}
