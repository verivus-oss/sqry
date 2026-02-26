//! `EdgeId` opaque handle for the unified graph architecture.
//!
//! This module implements `EdgeId`, a simple opaque handle type for edges.
//! Unlike `NodeId`, edges don't use generational indices since they are
//! identified by their (source, target, kind) tuple in the CSR format.
//!
//! # Design (FR-2)
//!
//! - **Opaque handle**: 32-bit index into edge storage
//! - **Simple equality**: No generation tracking (edges are immutable once created)
//! - **Memory efficient**: 4 bytes per ID

use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// Opaque edge identifier.
///
/// `EdgeId` provides a type-safe index into edge storage structures.
/// Edges in the unified graph are primarily identified by their
/// (source, target, kind) tuple, but `EdgeId` allows direct indexing
/// for efficient iteration and storage.
///
/// # Note
///
/// Unlike `NodeId`, `EdgeId` does not use generational indices because
/// edges are logically identified by their endpoints and kind. When an
/// edge is removed and recreated, it gets a new `EdgeId` but the same
/// logical identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EdgeId(u32);

impl EdgeId {
    /// Invalid sentinel value used to represent "no edge".
    pub const INVALID: EdgeId = EdgeId(u32::MAX);

    /// Creates a new `EdgeId` from a raw index.
    ///
    /// # Arguments
    ///
    /// * `index` - The storage index for this edge
    #[inline]
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Returns the raw index value.
    #[inline]
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// Returns the index as `usize` for array indexing.
    #[inline]
    #[must_use]
    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }

    /// Checks if this is the invalid sentinel value.
    #[inline]
    #[must_use]
    pub const fn is_invalid(self) -> bool {
        self.0 == u32::MAX
    }

    /// Checks if this is a valid (non-sentinel) ID.
    #[inline]
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.0 != u32::MAX
    }
}

impl fmt::Debug for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "EdgeId(INVALID)")
        } else {
            write!(f, "EdgeId({})", self.0)
        }
    }
}

impl fmt::Display for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "INVALID")
        } else {
            write!(f, "{}", self.0)
        }
    }
}

impl Default for EdgeId {
    /// Returns `EdgeId::INVALID` as the default value.
    #[inline]
    fn default() -> Self {
        Self::INVALID
    }
}

impl From<u32> for EdgeId {
    #[inline]
    fn from(index: u32) -> Self {
        Self(index)
    }
}

impl From<usize> for EdgeId {
    #[inline]
    fn from(index: usize) -> Self {
        Self(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edge_id_creation() {
        let id = EdgeId::new(42);
        assert_eq!(id.index(), 42);
        assert_eq!(id.as_usize(), 42);
        assert!(!id.is_invalid());
        assert!(id.is_valid());
    }

    #[test]
    fn test_edge_id_invalid_sentinel() {
        assert!(EdgeId::INVALID.is_invalid());
        assert!(!EdgeId::INVALID.is_valid());
        assert_eq!(EdgeId::INVALID.index(), u32::MAX);
    }

    #[test]
    fn test_edge_id_default() {
        let default_id: EdgeId = EdgeId::default();
        assert_eq!(default_id, EdgeId::INVALID);
    }

    #[test]
    fn test_edge_id_equality() {
        let id1 = EdgeId::new(5);
        let id2 = EdgeId::new(5);
        let id3 = EdgeId::new(6);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_edge_id_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(EdgeId::new(1));
        set.insert(EdgeId::new(2));
        set.insert(EdgeId::new(3));

        assert!(set.contains(&EdgeId::new(1)));
        assert!(!set.contains(&EdgeId::new(4)));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn test_edge_id_from() {
        let from_u32: EdgeId = 42u32.into();
        assert_eq!(from_u32.index(), 42);

        let from_usize: EdgeId = 42usize.into();
        assert_eq!(from_usize.index(), 42);
    }

    #[test]
    fn test_debug_display_format() {
        let id = EdgeId::new(42);
        assert_eq!(format!("{id:?}"), "EdgeId(42)");
        assert_eq!(format!("{id}"), "42");

        assert_eq!(format!("{:?}", EdgeId::INVALID), "EdgeId(INVALID)");
        assert_eq!(format!("{}", EdgeId::INVALID), "INVALID");
    }

    #[test]
    fn test_serde_roundtrip() {
        let original = EdgeId::new(123);

        // JSON roundtrip
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: EdgeId = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);

        // Postcard roundtrip
        let bytes = postcard::to_allocvec(&original).unwrap();
        let deserialized: EdgeId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_size_of_edge_id() {
        // Verify memory layout: u32 = 4 bytes
        assert_eq!(std::mem::size_of::<EdgeId>(), 4);
    }

    #[test]
    #[allow(clippy::clone_on_copy)] // Intentionally testing Clone trait
    fn test_copy_clone() {
        let id = EdgeId::new(10);
        let copied = id;
        let cloned = id.clone();

        assert_eq!(id, copied);
        assert_eq!(id, cloned);
    }
}
