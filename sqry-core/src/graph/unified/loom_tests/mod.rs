//! Loom-based formal concurrency verification tests.
//!
//! This module provides exhaustive concurrent testing using the loom model checker.
//! These tests verify correctness properties under all possible interleavings of
//! concurrent operations.
//!
//! # Test Coverage
//!
//! - **Step 28 (Admission)**: SharedBufferState atomic counter operations
//!   - Reserve + commit race
//!   - Concurrent reserve limit checking
//!   - Guard count underflow protection
//!   - Counter reset with active guards
//!
//! - **Step 29 (Compaction)**: Concurrent compaction safety
//!   - Delta buffer drain during compaction
//!   - Concurrent compaction triggers
//!   - Snapshot consistency during compaction
//!   - Counter synchronization post-compaction
//!   - Interrupted compaction recovery
//!
//! - **Step 30 (Concurrency)**: Graph concurrency primitives
//!   - Single-writer serialization via UpdateChannel
//!   - MVCC epoch consistency
//!
//! # Running Loom Tests
//!
//! Loom tests require the `loom` feature and should be run with a single thread:
//!
//! ```bash
//! RUSTFLAGS="--cfg loom" cargo test --features loom -p sqry-core loom_tests -- --test-threads=1
//! ```
//!
//! # Design Notes
//!
//! Loom explores all possible interleavings of concurrent operations, which can
//! lead to exponential state space. Tests are designed to minimize state space
//! while still covering critical paths:
//!
//! - Use small iteration counts (2-3 threads, 2-3 iterations)
//! - Focus on specific invariants rather than full functionality
//! - Use `loom::sync::atomic` instead of `std::sync::atomic`

mod admission;
mod compaction;
mod concurrency;
