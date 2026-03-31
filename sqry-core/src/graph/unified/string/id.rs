//! `StringId` opaque handle for the unified graph architecture.
//!
//! This module implements `StringId`, an opaque handle type for interned strings.
//! Strings are interned to reduce memory usage and enable O(1) equality comparison.
//!
//! # Design
//!
//! - **Opaque handle**: 32-bit index into string interner
//! - **Memory efficient**: 4 bytes per ID, shared storage for duplicate strings
//! - **Fast comparison**: O(1) equality via index comparison

use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// Opaque string identifier for interned strings.
///
/// `StringId` provides a type-safe index into the `StringInterner`.
/// All symbol names, file paths, and other strings are interned to reduce
/// memory usage and enable fast equality comparison.
///
/// # Thread Safety
///
/// `StringId` is `Copy` and `Send + Sync`. The actual string data lives
/// in the `StringInterner` which handles thread safety.
///
/// # Example
///
/// ```rust,ignore
/// let interner = StringInterner::new();
/// let id1 = interner.intern("hello");
/// let id2 = interner.intern("hello");
/// assert_eq!(id1, id2);  // Same string = same ID
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StringId(u32);

impl StringId {
    /// Bit used to tag staging-local `StringId`s.
    ///
    /// Staging-local `StringIds` are allocated by `GraphBuildHelper` and must be remapped
    /// via `StagingGraph::commit_strings()` + `StagingGraph::apply_string_remap()`
    /// before being committed to the main graph.
    ///
    /// Global (interner) `StringIds` MUST NEVER have this bit set.
    pub const LOCAL_TAG_BIT: u32 = 1 << 31;

    /// Invalid sentinel value used to represent "no string" or empty.
    pub const INVALID: StringId = StringId(u32::MAX);

    /// Creates a new `StringId` from a raw index.
    ///
    /// # Arguments
    ///
    /// * `index` - The interner index for this string
    ///
    /// # Safety Note
    ///
    /// This should only be called by the `StringInterner`. Using an index
    /// that doesn't correspond to an interned string will cause panics
    /// when resolving.
    #[inline]
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// Creates a new staging-local `StringId`.
    ///
    /// The returned `StringId` is guaranteed to be distinguishable from any
    /// global (interner) `StringId` by having [`Self::LOCAL_TAG_BIT`] set.
    #[inline]
    #[must_use]
    pub const fn new_local(local_index: u32) -> Self {
        Self(local_index | Self::LOCAL_TAG_BIT)
    }

    /// Returns `true` if this is a staging-local `StringId`.
    #[inline]
    #[must_use]
    pub const fn is_local(self) -> bool {
        !self.is_invalid() && (self.0 & Self::LOCAL_TAG_BIT) != 0
    }

    /// If this is a staging-local `StringId`, returns its local index.
    #[inline]
    #[must_use]
    pub const fn local_index(self) -> Option<u32> {
        if self.is_local() {
            Some(self.0 & !Self::LOCAL_TAG_BIT)
        } else {
            None
        }
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

impl fmt::Debug for StringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "StringId(INVALID)")
        } else if self.is_local() {
            write!(f, "StringId(local:{})", self.local_index().unwrap_or(0))
        } else {
            write!(f, "StringId({})", self.0)
        }
    }
}

impl fmt::Display for StringId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "INVALID")
        } else if self.is_local() {
            write!(f, "local:{}", self.local_index().unwrap_or(0))
        } else {
            write!(f, "str:{}", self.0)
        }
    }
}

impl Default for StringId {
    /// Returns `StringId::INVALID` as the default value.
    #[inline]
    fn default() -> Self {
        Self::INVALID
    }
}

impl From<u32> for StringId {
    #[inline]
    fn from(index: u32) -> Self {
        Self(index)
    }
}

impl From<usize> for StringId {
    #[inline]
    fn from(index: usize) -> Self {
        Self(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_id_creation() {
        let id = StringId::new(42);
        assert_eq!(id.index(), 42);
        assert_eq!(id.as_usize(), 42);
        assert!(!id.is_invalid());
        assert!(id.is_valid());
    }

    #[test]
    fn test_string_id_invalid_sentinel() {
        assert!(StringId::INVALID.is_invalid());
        assert!(!StringId::INVALID.is_valid());
        assert_eq!(StringId::INVALID.index(), u32::MAX);
    }

    #[test]
    fn test_string_id_default() {
        let default_id: StringId = StringId::default();
        assert_eq!(default_id, StringId::INVALID);
    }

    #[test]
    fn test_string_id_equality() {
        let id1 = StringId::new(5);
        let id2 = StringId::new(5);
        let id3 = StringId::new(6);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_string_id_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(StringId::new(1));
        set.insert(StringId::new(2));
        set.insert(StringId::new(3));

        assert!(set.contains(&StringId::new(1)));
        assert!(!set.contains(&StringId::new(4)));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn test_string_id_from() {
        let from_u32: StringId = 42u32.into();
        assert_eq!(from_u32.index(), 42);

        let from_usize: StringId = 42usize.into();
        assert_eq!(from_usize.index(), 42);
    }

    #[test]
    fn test_debug_display_format() {
        let id = StringId::new(42);
        assert_eq!(format!("{id:?}"), "StringId(42)");
        assert_eq!(format!("{id}"), "str:42");

        assert_eq!(format!("{:?}", StringId::INVALID), "StringId(INVALID)");
        assert_eq!(format!("{}", StringId::INVALID), "INVALID");
    }

    #[test]
    fn test_serde_roundtrip() {
        let original = StringId::new(123);

        // JSON roundtrip
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: StringId = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);

        // Postcard roundtrip
        let bytes = postcard::to_allocvec(&original).unwrap();
        let deserialized: StringId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_size_of_string_id() {
        // Verify memory layout: u32 = 4 bytes
        assert_eq!(std::mem::size_of::<StringId>(), 4);
    }

    #[test]
    #[allow(clippy::clone_on_copy)] // Intentionally testing Clone trait
    fn test_copy_clone() {
        let id = StringId::new(10);
        let copied = id;
        let cloned = id.clone();

        assert_eq!(id, copied);
        assert_eq!(id, cloned);
    }
}
