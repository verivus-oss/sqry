//! Heap memory size reporting for graph data structures.
//!
//! The [`GraphMemorySize`] trait provides a uniform way to estimate heap
//! memory owned by graph components. Implementations sum
//! `Vec::capacity() * size_of::<T>()` for all heap-allocated collections,
//! plus any `Box`/`Arc`/`HashMap` overhead.
//!
//! Used by the `sqryd` daemon's admission controller and workspace
//! retention reaper to enforce memory budgets.

/// Approximate per-bucket overhead for `HashMap` entries.
///
/// Rust's `hashbrown` stores one control byte per bucket plus alignment
/// padding. 8 bytes per bucket is a conservative estimate that accounts
/// for the control byte array, growth factor headroom, and pointer-sized
/// alignment on 64-bit targets.
pub const HASHMAP_ENTRY_OVERHEAD: usize = 8;

/// Approximate per-node overhead for `BTreeMap` entries.
///
/// `BTreeMap` stores entries in B-tree nodes (typically 11 entries per
/// internal node). Each node carries child pointers and metadata. 64
/// bytes per logical entry is a rough estimate that accounts for node
/// overhead amortised across entries.
pub const BTREEMAP_ENTRY_OVERHEAD: usize = 64;

/// Trait for reporting heap memory usage of graph data structures.
///
/// Implementations should sum `Vec::capacity() * size_of::<T>()` for all
/// heap-allocated collections, plus any `Box`/`Arc` overhead. Only heap
/// bytes are counted; stack/inline bytes are excluded.
///
/// # Contract
///
/// - Returns an *estimate*, not an exact count. Allocator metadata,
///   alignment padding, and `Arc` control blocks are not included.
/// - Must be safe to call concurrently from multiple readers.
/// - Should complete in O(number of collections), not O(number of elements).
pub trait GraphMemorySize {
    /// Returns estimated heap bytes owned by this structure.
    fn heap_bytes(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vec_memory_size() {
        let v: Vec<u64> = Vec::with_capacity(100);
        assert_eq!(v.capacity() * std::mem::size_of::<u64>(), 800);
    }

    #[test]
    fn constants_are_nonzero() {
        // Use runtime binding to prevent compile-time constant folding.
        let hm = std::hint::black_box(HASHMAP_ENTRY_OVERHEAD);
        let bt = std::hint::black_box(BTREEMAP_ENTRY_OVERHEAD);
        assert!(hm > 0);
        assert!(bt > 0);
    }
}
