//! Loom-based formal concurrency verification tests.
//!
//! This module provides exhaustive concurrent testing using the loom model checker.
//! These tests verify correctness properties under all possible interleavings of
//! concurrent operations.
//!
//! # Test Coverage
//!
//! - **Step 28 (Admission)**: SharedBufferState atomic counter operations
//!   - CP-16: Reserve + commit race
//!   - CP-17: Concurrent reserve limit checking
//!   - CP-18: Guard count underflow protection
//!   - CP-19: Counter reset with active guards
//!
//! - **Step 29 (Compaction)**: Concurrent compaction safety
//!   - CP-11: Delta buffer drain during compaction
//!   - CP-12: Concurrent compaction triggers
//!   - CP-13: Snapshot consistency during compaction
//!   - CP-14: Counter synchronization post-compaction
//!   - CP-15: Interrupted compaction recovery
//!
//! - **Step 30 (Concurrency)**: Graph concurrency primitives
//!   - CP-5: Single-writer serialization via UpdateChannel
//!   - CP-6: MVCC epoch consistency
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
