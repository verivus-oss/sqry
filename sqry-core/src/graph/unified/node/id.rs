//! `NodeId` with generational index for the unified graph architecture.
//!
//! This module implements `NodeId`, an opaque handle type that uses generational
//! indices to detect stale references after node deletion.
//!
//! # Design (FR-1, FR-32, FR-48)
//!
//! - **u32 index**: Supports up to 4 billion nodes (sufficient for any project)
//! - **u64 generation**: Prevents wrap-around in practical use (~9 quintillion operations)
//! - **Stale detection**: Lookups validate generation matches arena slot
//! - **Overflow guard**: `checked_add` returns error if overflow imminent
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::node::NodeId;
//!
//! let id = NodeId::new(42, 1);
//! assert!(!id.is_invalid());
//! assert_eq!(id.index(), 42);
//! assert_eq!(id.generation(), 1);
//! ```

use std::fmt;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// Error returned when generation counter would overflow.
///
/// This is a safety mechanism to prevent generation wrap-around which could
/// cause a new node to appear valid when checked against a stale `NodeId`.
/// In practice, this should never occur as u64 provides ~9 quintillion operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationOverflowError {
    /// The index of the arena slot that overflowed
    pub index: u32,
    /// The generation value at overflow
    pub generation: u64,
}

impl fmt::Display for GenerationOverflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "generation overflow at index {}: generation {} would exceed MAX_GENERATION",
            self.index, self.generation
        )
    }
}

impl std::error::Error for GenerationOverflowError {}

/// Generational node identifier with overflow protection.
///
/// `NodeId` provides a type-safe, generational index into the `NodeArena`.
/// The generation counter ensures that stale references (to deleted nodes)
/// are detected and rejected.
///
/// # Memory Layout
///
/// ```text
/// NodeId (12 bytes on 64-bit systems):
/// ┌──────────────────────────────────────────┐
/// │ index: u32      │ generation: u64        │
/// │ (4 bytes)       │ (8 bytes)              │
/// └──────────────────────────────────────────┘
/// ```
///
/// # Generational Index Pattern
///
/// When a node is removed from the arena, the slot's generation is incremented.
/// Any `NodeId` holding the old generation will fail validation when used for lookup.
///
/// ```text
/// Time 0: Allocate slot 5, generation 1 → NodeId { index: 5, generation: 1 }
/// Time 1: Remove slot 5 → slot generation becomes 2
/// Time 2: Lookup with NodeId { index: 5, generation: 1 } → FAILS (1 ≠ 2)
/// Time 3: Allocate slot 5, generation 2 → NodeId { index: 5, generation: 2 }
/// Time 4: Lookup with NodeId { index: 5, generation: 2 } → SUCCEEDS
/// ```
#[derive(Clone, Copy, Serialize, Deserialize, Ord, PartialOrd)]
pub struct NodeId {
    /// Index into `NodeArena` slots vector.
    index: u32,
    /// Generation counter - incremented on slot reuse.
    generation: u64,
}

impl NodeId {
    /// Invalid sentinel value used to represent "no node".
    ///
    /// Uses `u32::MAX` as index which is reserved and never allocated.
    pub const INVALID: NodeId = NodeId {
        index: u32::MAX,
        generation: 0,
    };

    /// Maximum safe generation value before overflow protection kicks in.
    ///
    /// Set to `u64::MAX / 2` (~9.2 quintillion) to provide a safety margin.
    /// This allows for detection before actual overflow occurs.
    pub const MAX_GENERATION: u64 = u64::MAX / 2;

    /// Creates a new `NodeId` with the given index and generation.
    ///
    /// # Arguments
    ///
    /// * `index` - The slot index in the arena
    /// * `generation` - The generation counter for this slot
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let id = NodeId::new(0, 1);
    /// assert_eq!(id.index(), 0);
    /// assert_eq!(id.generation(), 1);
    /// ```
    #[inline]
    #[must_use]
    pub const fn new(index: u32, generation: u64) -> Self {
        Self { index, generation }
    }

    /// Returns the slot index.
    #[inline]
    #[must_use]
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Returns the generation counter.
    #[inline]
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Checks if this is the invalid sentinel value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// assert!(NodeId::INVALID.is_invalid());
    /// assert!(!NodeId::new(0, 1).is_invalid());
    /// ```
    #[inline]
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.index == u32::MAX
    }

    /// Checks if this is a valid (non-sentinel) ID.
    ///
    /// Note: This only checks the sentinel value, not whether the ID
    /// actually refers to a live node in an arena.
    #[inline]
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.index != u32::MAX
    }

    /// Attempts to increment the generation, returning an error if overflow would occur.
    ///
    /// This is used by `NodeArena` when a slot is freed and will be reused.
    ///
    /// # Errors
    ///
    /// Returns `GenerationOverflowError` if the generation would exceed `MAX_GENERATION`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let id = NodeId::new(5, 1);
    /// let next_gen = id.try_increment_generation()?;
    /// assert_eq!(next_gen, 2);
    /// ```
    pub fn try_increment_generation(self) -> Result<u64, GenerationOverflowError> {
        let next = self
            .generation
            .checked_add(1)
            .ok_or(GenerationOverflowError {
                index: self.index,
                generation: self.generation,
            })?;

        if next > Self::MAX_GENERATION {
            return Err(GenerationOverflowError {
                index: self.index,
                generation: self.generation,
            });
        }

        Ok(next)
    }

    /// Checks if the generation is approaching the overflow limit.
    ///
    /// Returns `true` if generation is within 1000 of `MAX_GENERATION`.
    /// This can be used for proactive warnings.
    #[inline]
    #[must_use]
    pub const fn is_near_overflow(self) -> bool {
        self.generation > Self::MAX_GENERATION.saturating_sub(1000)
    }
}

impl PartialEq for NodeId {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl Eq for NodeId {}

impl Hash for NodeId {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Combine index and generation into hash for good distribution
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "NodeId(INVALID)")
        } else {
            write!(f, "NodeId({}:{})", self.index, self.generation)
        }
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "INVALID")
        } else {
            write!(f, "{}:{}", self.index, self.generation)
        }
    }
}

impl Default for NodeId {
    /// Returns `NodeId::INVALID` as the default value.
    #[inline]
    fn default() -> Self {
        Self::INVALID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_id_creation() {
        let id = NodeId::new(42, 1);
        assert_eq!(id.index(), 42);
        assert_eq!(id.generation(), 1);
        assert!(!id.is_invalid());
        assert!(id.is_valid());
    }

    #[test]
    fn test_node_id_invalid_sentinel() {
        assert!(NodeId::INVALID.is_invalid());
        assert!(!NodeId::INVALID.is_valid());
        assert_eq!(NodeId::INVALID.index(), u32::MAX);
        assert_eq!(NodeId::INVALID.generation(), 0);
    }

    #[test]
    fn test_node_id_default() {
        let default_id: NodeId = NodeId::default();
        assert_eq!(default_id, NodeId::INVALID);
    }

    #[test]
    fn test_node_id_equality() {
        let id1 = NodeId::new(5, 10);
        let id2 = NodeId::new(5, 10);
        let id3 = NodeId::new(5, 11);
        let id4 = NodeId::new(6, 10);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3); // Different generation
        assert_ne!(id1, id4); // Different index
    }

    #[test]
    fn test_node_id_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(NodeId::new(1, 1));
        set.insert(NodeId::new(1, 2));
        set.insert(NodeId::new(2, 1));

        assert!(set.contains(&NodeId::new(1, 1)));
        assert!(set.contains(&NodeId::new(1, 2)));
        assert!(set.contains(&NodeId::new(2, 1)));
        assert!(!set.contains(&NodeId::new(3, 1)));
        assert_eq!(set.len(), 3);
    }

    #[test]
    #[allow(clippy::clone_on_copy)] // Intentionally testing Clone trait
    fn test_node_id_copy_clone() {
        let id = NodeId::new(10, 20);
        let copied = id;
        let cloned = id.clone();

        assert_eq!(id, copied);
        assert_eq!(id, cloned);
    }

    #[test]
    fn test_generation_increment_success() {
        let id = NodeId::new(5, 1);
        let next_gen = id.try_increment_generation().unwrap();
        assert_eq!(next_gen, 2);
    }

    #[test]
    fn test_generation_increment_at_max() {
        let id = NodeId::new(5, NodeId::MAX_GENERATION);
        let result = id.try_increment_generation();
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.index, 5);
        assert_eq!(err.generation, NodeId::MAX_GENERATION);
    }

    #[test]
    fn test_generation_overflow_at_u64_max() {
        let id = NodeId::new(5, u64::MAX);
        let result = id.try_increment_generation();
        assert!(result.is_err());
    }

    #[test]
    fn test_is_near_overflow() {
        let safe_id = NodeId::new(5, 1000);
        assert!(!safe_id.is_near_overflow());

        let near_limit = NodeId::new(5, NodeId::MAX_GENERATION - 500);
        assert!(near_limit.is_near_overflow());

        let at_limit = NodeId::new(5, NodeId::MAX_GENERATION);
        assert!(at_limit.is_near_overflow());
    }

    #[test]
    fn test_debug_display_format() {
        let id = NodeId::new(42, 7);
        assert_eq!(format!("{id:?}"), "NodeId(42:7)");
        assert_eq!(format!("{id}"), "42:7");

        assert_eq!(format!("{:?}", NodeId::INVALID), "NodeId(INVALID)");
        assert_eq!(format!("{}", NodeId::INVALID), "INVALID");
    }

    #[test]
    fn test_serde_roundtrip() {
        let original = NodeId::new(123, 456);

        // Test JSON serialization
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: NodeId = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);

        // Test postcard serialization
        let bytes = postcard::to_allocvec(&original).unwrap();
        let deserialized: NodeId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_size_of_node_id() {
        // Verify memory layout: u32 (4 bytes) + u64 (8 bytes) = 12 bytes
        // With padding, this may be 16 bytes on some platforms
        assert!(std::mem::size_of::<NodeId>() <= 16);
        assert!(std::mem::size_of::<NodeId>() >= 12);
    }
}
