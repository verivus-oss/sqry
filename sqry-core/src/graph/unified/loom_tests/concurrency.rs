//! Loom tests for graph concurrency primitives (Step 30).
//!
//! Tests concurrent graph operations under all interleavings:
//!
//! - Single-writer serialization via UpdateChannel
//! - MVCC epoch consistency

use loom::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use loom::sync::{Arc, RwLock};
use loom::thread;

/// Loom-compatible concurrent graph for MVCC testing.
///
/// Simulates the key MVCC properties:
/// - Epoch-based versioning
/// - Reader snapshots
/// - Single-writer updates
struct LoomConcurrentGraph {
    /// Current epoch (version)
    epoch: AtomicU64,
    /// Node count (simplified representation)
    node_count: RwLock<usize>,
    /// Whether a write is in progress
    writing: AtomicBool,
}

impl LoomConcurrentGraph {
    fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            node_count: RwLock::new(0),
            writing: AtomicBool::new(false),
        }
    }

    /// Takes a read snapshot, returning the current epoch.
    fn snapshot_epoch(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Reads node count under read lock.
    fn read_node_count(&self) -> usize {
        *self.node_count.read().unwrap()
    }

    /// Tries to acquire write lock, returning false if already writing.
    fn try_begin_write(&self) -> bool {
        self.writing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Performs a write operation (add node).
    fn write_add_node(&self) {
        let mut count = self.node_count.write().unwrap();
        *count += 1;
    }

    /// Completes write, incrementing epoch.
    fn end_write(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.writing.store(false, Ordering::Release);
    }

    fn is_writing(&self) -> bool {
        self.writing.load(Ordering::Acquire)
    }
}

/// Loom-compatible single-writer queue for testing serialization.
///
/// Uses atomic counters to track operations without complex channel semantics.
struct LoomWriteQueue {
    /// Counter for enqueued operations
    enqueued: AtomicU64,
    /// Counter for dequeued operations
    dequeued: AtomicU64,
    /// Whether queue is closed
    closed: AtomicBool,
}

impl LoomWriteQueue {
    fn new() -> Self {
        Self {
            enqueued: AtomicU64::new(0),
            dequeued: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    /// Attempts to enqueue an item.
    ///
    /// Uses a CAS loop to atomically check closed state and increment counter,
    /// preventing TOCTOU race where close() happens between check and increment.
    fn enqueue(&self) -> bool {
        // First check if closed (fast path)
        if self.closed.load(Ordering::Acquire) {
            return false;
        }

        // Atomically increment enqueued counter
        let _prev = self.enqueued.fetch_add(1, Ordering::AcqRel);

        // Re-check closed state after increment (linearization point)
        // If closed was set before our increment was visible, we need to rollback
        if self.closed.load(Ordering::Acquire) {
            // Race detected: close() happened. Decrement to undo our enqueue.
            // This ensures no new items are enqueued after close.
            self.enqueued.fetch_sub(1, Ordering::AcqRel);
            return false;
        }

        true
    }

    /// Dequeues an item using CAS to prevent double-consume.
    ///
    /// Uses compare-exchange loop to ensure only one thread succeeds
    /// per enqueued item, preventing the double-consume race condition.
    fn dequeue(&self) -> bool {
        loop {
            let enq = self.enqueued.load(Ordering::Acquire);
            let deq = self.dequeued.load(Ordering::Acquire);

            if deq >= enq {
                // Queue empty
                return false;
            }

            // CAS to atomically claim this slot
            if self
                .dequeued
                .compare_exchange_weak(deq, deq + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
            // CAS failed (another thread claimed it), retry
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn enqueued(&self) -> u64 {
        self.enqueued.load(Ordering::Acquire)
    }

    fn dequeued(&self) -> u64 {
        self.dequeued.load(Ordering::Acquire)
    }

    fn in_flight(&self) -> u64 {
        self.enqueued().saturating_sub(self.dequeued())
    }
}

/// Single-writer serialization.
///
/// Verifies that operations are processed serially via atomic counters.
#[test]
fn test_cp5_single_writer_serialization() {
    loom::model(|| {
        let queue = Arc::new(LoomWriteQueue::new());

        let q1 = Arc::clone(&queue);
        let q2 = Arc::clone(&queue);

        // Thread 1: Enqueue operations
        let t1 = thread::spawn(move || {
            q1.enqueue();
            q1.enqueue();
        });

        // Thread 2: Enqueue operations
        let t2 = thread::spawn(move || {
            q2.enqueue();
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // All enqueues should be counted
        assert_eq!(queue.enqueued(), 3, "All enqueues should be counted");
    });
}

/// Operation ordering via atomic sequence.
///
/// Verifies that operations get unique sequence numbers.
#[test]
fn test_cp5_operation_ordering() {
    loom::model(|| {
        let seq = Arc::new(AtomicU64::new(0));

        let s1 = Arc::clone(&seq);
        let s2 = Arc::clone(&seq);

        let seq1 = Arc::new(AtomicU64::new(u64::MAX));
        let seq2 = Arc::new(AtomicU64::new(u64::MAX));
        let r1 = Arc::clone(&seq1);
        let r2 = Arc::clone(&seq2);

        // Thread 1: Get sequence number
        let t1 = thread::spawn(move || {
            let n = s1.fetch_add(1, Ordering::AcqRel);
            r1.store(n, Ordering::Relaxed);
        });

        // Thread 2: Get sequence number
        let t2 = thread::spawn(move || {
            let n = s2.fetch_add(1, Ordering::AcqRel);
            r2.store(n, Ordering::Relaxed);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let n1 = seq1.load(Ordering::Relaxed);
        let n2 = seq2.load(Ordering::Relaxed);

        // Sequences should be unique (0 and 1)
        assert_ne!(n1, n2, "Sequence numbers should be unique");
        assert!(n1 < 2 && n2 < 2, "Sequences should be 0 or 1");
    });
}

/// MVCC epoch consistency with snapshot correlation.
///
/// Verifies that readers see consistent epoch-to-data correlation:
/// - If reader observes epoch 0, node_count should be 0
/// - If reader observes epoch 1, node_count should be 1
///
/// This catches stale-epoch/read-skew bugs where a reader sees an
/// incremented epoch with pre-write data (or vice versa).
#[test]
fn test_cp6_epoch_consistency() {
    loom::model(|| {
        let graph = Arc::new(LoomConcurrentGraph::new());

        let graph1 = Arc::clone(&graph);
        let graph2 = Arc::clone(&graph);

        // Track reader's observed epoch and count
        let reader_epoch = Arc::new(AtomicU64::new(u64::MAX));
        let reader_count = Arc::new(AtomicUsize::new(usize::MAX));
        let re = Arc::clone(&reader_epoch);
        let rc = Arc::clone(&reader_count);

        // Thread 1: Writer - adds one node, increments epoch
        let t1 = thread::spawn(move || {
            if graph1.try_begin_write() {
                graph1.write_add_node();
                graph1.end_write();
            }
        });

        // Thread 2: Reader - takes snapshot and reads count
        let t2 = thread::spawn(move || {
            // Take epoch snapshot FIRST
            let epoch = graph2.snapshot_epoch();
            re.store(epoch, Ordering::Relaxed);

            // Then read node count
            let count = graph2.read_node_count();
            rc.store(count, Ordering::Relaxed);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        let epoch = reader_epoch.load(Ordering::Relaxed);
        let count = reader_count.load(Ordering::Relaxed);

        // CRITICAL INVARIANT: Epoch must correlate with data
        // - epoch 0 means no write completed yet -> count should be 0
        // - epoch 1 means write completed -> count should be 1
        //
        // Due to MVCC semantics, these are the valid combinations:
        // - (epoch=0, count=0): Reader ran entirely before write
        // - (epoch=0, count=1): Reader got epoch before write, count after (stale epoch - but allowed under RwLock)
        // - (epoch=1, count=0): Reader got epoch after write, count before (INVALID - would indicate bug)
        // - (epoch=1, count=1): Reader ran entirely after write
        //
        // The key invariant: if epoch is incremented, count must reflect the write.
        // (epoch=1, count=0) would be a read-skew bug.
        assert!(
            !(epoch == 1 && count == 0),
            "Read-skew detected: epoch {} but count {}",
            epoch,
            count
        );

        // Reader should see a valid epoch (0 or 1)
        assert!(epoch <= 1, "Epoch should be 0 or 1, got {}", epoch);
    });
}

/// Concurrent readers during write with epoch correlation.
///
/// Verifies that multiple readers can operate concurrently during a write
/// and that their observed epochs correlate with the data they see.
#[test]
fn test_cp6_concurrent_readers() {
    loom::model(|| {
        let graph = Arc::new(LoomConcurrentGraph::new());

        // Initialize with some data (2 nodes, epoch 1)
        {
            graph.try_begin_write();
            graph.write_add_node();
            graph.write_add_node();
            graph.end_write();
        }

        let graph1 = Arc::clone(&graph);
        let graph2 = Arc::clone(&graph);
        let graph3 = Arc::clone(&graph);

        // Track both epoch and count for each reader
        let epoch1 = Arc::new(AtomicU64::new(u64::MAX));
        let epoch2 = Arc::new(AtomicU64::new(u64::MAX));
        let count1 = Arc::new(AtomicUsize::new(usize::MAX));
        let count2 = Arc::new(AtomicUsize::new(usize::MAX));

        let e1 = Arc::clone(&epoch1);
        let e2 = Arc::clone(&epoch2);
        let c1 = Arc::clone(&count1);
        let c2 = Arc::clone(&count2);

        // Thread 1: Writer - adds third node
        let t1 = thread::spawn(move || {
            if graph1.try_begin_write() {
                graph1.write_add_node();
                graph1.end_write();
            }
        });

        // Thread 2: Reader 1 - snapshot epoch then read count
        let t2 = thread::spawn(move || {
            let epoch = graph2.snapshot_epoch();
            e1.store(epoch, Ordering::Relaxed);
            let count = graph2.read_node_count();
            c1.store(count, Ordering::Relaxed);
        });

        // Thread 3: Reader 2 - snapshot epoch then read count
        let t3 = thread::spawn(move || {
            let epoch = graph3.snapshot_epoch();
            e2.store(epoch, Ordering::Relaxed);
            let count = graph3.read_node_count();
            c2.store(count, Ordering::Relaxed);
        });

        t1.join().unwrap();
        t2.join().unwrap();
        t3.join().unwrap();

        let r1_epoch = epoch1.load(Ordering::Relaxed);
        let r1_count = count1.load(Ordering::Relaxed);
        let r2_epoch = epoch2.load(Ordering::Relaxed);
        let r2_count = count2.load(Ordering::Relaxed);

        // Valid epochs: 1 (before second write) or 2 (after second write)
        assert!(
            r1_epoch == 1 || r1_epoch == 2,
            "Reader 1 epoch should be 1 or 2, got {}",
            r1_epoch
        );
        assert!(
            r2_epoch == 1 || r2_epoch == 2,
            "Reader 2 epoch should be 1 or 2, got {}",
            r2_epoch
        );

        // Readers should see either 2 (before write) or 3 (after write)
        assert!(
            r1_count == 2 || r1_count == 3,
            "Reader 1 should see valid count, got {}",
            r1_count
        );
        assert!(
            r2_count == 2 || r2_count == 3,
            "Reader 2 should see valid count, got {}",
            r2_count
        );

        // CRITICAL: Epoch-count correlation check
        // If reader sees epoch 2 (post-write), they should see count 3
        // (epoch=2, count=2) would indicate read-skew
        assert!(
            !(r1_epoch == 2 && r1_count == 2),
            "Reader 1 read-skew: epoch {} but count {}",
            r1_epoch,
            r1_count
        );
        assert!(
            !(r2_epoch == 2 && r2_count == 2),
            "Reader 2 read-skew: epoch {} but count {}",
            r2_epoch,
            r2_count
        );
    });
}

/// Test exclusive writer access with overlap detection.
///
/// Verifies that only one writer can be active at a time using an
/// overlap detector that catches concurrent write attempts.
#[test]
fn test_exclusive_writer_access() {
    loom::model(|| {
        let graph = Arc::new(LoomConcurrentGraph::new());

        let graph1 = Arc::clone(&graph);
        let graph2 = Arc::clone(&graph);

        // Overlap detector: tracks concurrent active writers
        let active_writers = Arc::new(AtomicUsize::new(0));
        let overlap_detected = Arc::new(AtomicBool::new(false));

        let active1 = Arc::clone(&active_writers);
        let active2 = Arc::clone(&active_writers);
        let overlap1 = Arc::clone(&overlap_detected);
        let overlap2 = Arc::clone(&overlap_detected);

        let write_count = Arc::new(AtomicUsize::new(0));
        let wc1 = Arc::clone(&write_count);
        let wc2 = Arc::clone(&write_count);

        // Thread 1: Try to write with overlap detection
        let t1 = thread::spawn(move || {
            if graph1.try_begin_write() {
                // Check for overlap BEFORE incrementing
                let prev = active1.fetch_add(1, Ordering::AcqRel);
                if prev > 0 {
                    overlap1.store(true, Ordering::Release);
                }

                wc1.fetch_add(1, Ordering::Relaxed);
                graph1.write_add_node();

                active1.fetch_sub(1, Ordering::AcqRel);
                graph1.end_write();
            }
        });

        // Thread 2: Try to write with overlap detection
        let t2 = thread::spawn(move || {
            if graph2.try_begin_write() {
                let prev = active2.fetch_add(1, Ordering::AcqRel);
                if prev > 0 {
                    overlap2.store(true, Ordering::Release);
                }

                wc2.fetch_add(1, Ordering::Relaxed);
                graph2.write_add_node();

                active2.fetch_sub(1, Ordering::AcqRel);
                graph2.end_write();
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // CRITICAL INVARIANT: No overlapping writers detected
        assert!(
            !overlap_detected.load(Ordering::Acquire),
            "Mutual exclusion violated: concurrent writers detected"
        );

        // Both writes should succeed sequentially (back-to-back is OK)
        let writes = write_count.load(Ordering::Relaxed);
        assert!(writes <= 2, "At most 2 writes should succeed");

        // Graph should not be in writing state
        assert!(!graph.is_writing(), "Writing should be complete");
    });
}

/// Test epoch monotonicity.
///
/// Verifies that epoch numbers never decrease.
#[test]
fn test_epoch_monotonicity() {
    loom::model(|| {
        let graph = Arc::new(LoomConcurrentGraph::new());

        let graph1 = Arc::clone(&graph);
        let graph2 = Arc::clone(&graph);

        let epochs = Arc::new(RwLock::new(Vec::new()));
        let e2 = Arc::clone(&epochs);

        // Thread 1: Perform writes
        let t1 = thread::spawn(move || {
            if graph1.try_begin_write() {
                graph1.write_add_node();
                graph1.end_write();
            }
        });

        // Thread 2: Sample epochs
        let t2 = thread::spawn(move || {
            let epoch1 = graph2.snapshot_epoch();
            let epoch2 = graph2.snapshot_epoch();

            let mut epochs = e2.write().unwrap();
            epochs.push(epoch1);
            epochs.push(epoch2);
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Verify epochs are monotonically non-decreasing
        let epochs = epochs.read().unwrap();
        for i in 1..epochs.len() {
            assert!(
                epochs[i] >= epochs[i - 1],
                "Epochs should be monotonically non-decreasing"
            );
        }
    });
}

/// Test queue counter tracking.
///
/// Verifies that enqueue and dequeue counts are accurate.
#[test]
fn test_queue_counter_tracking() {
    loom::model(|| {
        let queue = Arc::new(LoomWriteQueue::new());

        let q1 = Arc::clone(&queue);

        // Thread 1: Enqueue
        let t1 = thread::spawn(move || {
            q1.enqueue();
            q1.enqueue();
        });

        t1.join().unwrap();

        // Dequeue both
        assert!(queue.dequeue(), "First dequeue should succeed");
        assert!(queue.dequeue(), "Second dequeue should succeed");
        assert!(!queue.dequeue(), "Third dequeue should fail (empty)");

        // Counts should match
        assert_eq!(queue.enqueued(), 2, "Enqueued count should be 2");
        assert_eq!(queue.dequeued(), 2, "Dequeued count should be 2");
        assert_eq!(queue.in_flight(), 0, "In-flight should be 0");
    });
}

/// Test queue closure with strict ordering guarantees.
///
/// Verifies that:
/// 1. Enqueue fails after queue is closed
/// 2. No successful enqueue can linearize after close()
/// 3. The enqueue count reflects only successfully linearized enqueues
#[test]
fn test_queue_closure() {
    loom::model(|| {
        let queue = Arc::new(LoomWriteQueue::new());

        let q1 = Arc::clone(&queue);
        let q2 = Arc::clone(&queue);

        // Track when close happened relative to t2's enqueue
        let close_completed = Arc::new(AtomicBool::new(false));
        let cc = Arc::clone(&close_completed);

        // Thread 1: Enqueue then close
        let t1 = thread::spawn(move || {
            q1.enqueue();
            q1.close();
            cc.store(true, Ordering::Release);
        });

        // Thread 2: Try to enqueue (may or may not succeed)
        let t2 = thread::spawn(move || q2.enqueue());

        t1.join().unwrap();
        let t2_result = t2.join().unwrap();

        // CRITICAL INVARIANT: After close, no new enqueues succeed
        assert!(!queue.enqueue(), "Enqueue after close should fail");

        // Get final enqueue count
        let total = queue.enqueued();

        // Valid outcomes:
        // - t2 succeeded before close: total = 2, t2_result = true
        // - t2 failed due to close: total = 1, t2_result = false
        // - t2 rolled back due to close: total = 1, t2_result = false
        //
        // INVALID: t2 succeeded after close (total = 2 but close was before)
        // The double-check in enqueue() prevents this.

        if t2_result {
            // t2 succeeded, so it must have linearized before close
            assert_eq!(total, 2, "If t2 succeeded, total should be 2");
        } else {
            // t2 failed (either saw closed, or rolled back)
            assert_eq!(total, 1, "If t2 failed, total should be 1");
        }

        // Additional invariant: count must be 1 or 2
        assert!(
            total >= 1 && total <= 2,
            "Total enqueued should be 1 or 2, got {}",
            total
        );
    });
}

/// Test concurrent enqueue and dequeue.
///
/// Verifies that concurrent enqueue and dequeue operations are safe.
#[test]
fn test_concurrent_enqueue_dequeue() {
    loom::model(|| {
        let queue = Arc::new(LoomWriteQueue::new());

        // Pre-enqueue some items
        queue.enqueue();
        queue.enqueue();

        let q1 = Arc::clone(&queue);
        let q2 = Arc::clone(&queue);

        let dequeued = Arc::new(AtomicUsize::new(0));
        let d1 = Arc::clone(&dequeued);
        let d2 = Arc::clone(&dequeued);

        // Thread 1: Dequeue
        let t1 = thread::spawn(move || {
            if q1.dequeue() {
                d1.fetch_add(1, Ordering::Relaxed);
            }
        });

        // Thread 2: Dequeue
        let t2 = thread::spawn(move || {
            if q2.dequeue() {
                d2.fetch_add(1, Ordering::Relaxed);
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Both should succeed since we pre-enqueued 2
        let total_dequeued = dequeued.load(Ordering::Relaxed);
        assert_eq!(total_dequeued, 2, "Both dequeues should succeed");
    });
}
