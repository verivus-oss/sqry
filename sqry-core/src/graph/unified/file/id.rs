//! `FileId` opaque handle for the unified graph architecture.
//!
//! This module implements `FileId`, an opaque handle type for registered file paths.
//! Files are registered in the `FileRegistry` to deduplicate path storage and
//! enable efficient file-based lookups.
//!
//! # Design
//!
//! - **Opaque handle**: 32-bit index into file registry
//! - **Path deduplication**: Same canonical path returns same `FileId`
//! - **Memory efficient**: 4 bytes per ID, shared storage for paths

use std::fmt;
use std::hash::Hash;

use serde::{Deserialize, Serialize};

/// Opaque file identifier for registered file paths.
///
/// `FileId` provides a type-safe index into the `FileRegistry`.
/// All file paths are canonicalized and deduplicated to reduce memory
/// usage and enable fast file-based lookups.
///
/// # Thread Safety
///
/// `FileId` is `Copy` and `Send + Sync`. The actual path data lives
/// in the `FileRegistry` which handles thread safety.
///
/// # Example
///
/// ```rust,ignore
/// let registry = FileRegistry::new();
/// let id1 = registry.register("/home/user/project/src/main.rs")?;
/// let id2 = registry.register("/home/user/project/src/main.rs")?;
/// assert_eq!(id1, id2);  // Same path = same ID
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FileId(u32);

impl FileId {
    /// Invalid sentinel value used to represent "no file" or unknown file.
    pub const INVALID: FileId = FileId(u32::MAX);

    /// Creates a new `FileId` from a raw index.
    ///
    /// # Arguments
    ///
    /// * `index` - The registry index for this file
    ///
    /// # Safety Note
    ///
    /// This should only be called by the `FileRegistry`. Using an index
    /// that doesn't correspond to a registered file will cause panics
    /// when resolving.
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

impl fmt::Debug for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "FileId(INVALID)")
        } else {
            write!(f, "FileId({})", self.0)
        }
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_invalid() {
            write!(f, "INVALID")
        } else {
            write!(f, "file:{}", self.0)
        }
    }
}

impl Default for FileId {
    /// Returns `FileId::INVALID` as the default value.
    #[inline]
    fn default() -> Self {
        Self::INVALID
    }
}

impl From<u32> for FileId {
    #[inline]
    fn from(index: u32) -> Self {
        Self(index)
    }
}

impl From<usize> for FileId {
    #[inline]
    fn from(index: usize) -> Self {
        Self(u32::try_from(index).unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_id_creation() {
        let id = FileId::new(42);
        assert_eq!(id.index(), 42);
        assert_eq!(id.as_usize(), 42);
        assert!(!id.is_invalid());
        assert!(id.is_valid());
    }

    #[test]
    fn test_file_id_invalid_sentinel() {
        assert!(FileId::INVALID.is_invalid());
        assert!(!FileId::INVALID.is_valid());
        assert_eq!(FileId::INVALID.index(), u32::MAX);
    }

    #[test]
    fn test_file_id_default() {
        let default_id: FileId = FileId::default();
        assert_eq!(default_id, FileId::INVALID);
    }

    #[test]
    fn test_file_id_equality() {
        let id1 = FileId::new(5);
        let id2 = FileId::new(5);
        let id3 = FileId::new(6);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_file_id_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(FileId::new(1));
        set.insert(FileId::new(2));
        set.insert(FileId::new(3));

        assert!(set.contains(&FileId::new(1)));
        assert!(!set.contains(&FileId::new(4)));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn test_file_id_from() {
        let from_u32: FileId = 42u32.into();
        assert_eq!(from_u32.index(), 42);

        let from_usize: FileId = 42usize.into();
        assert_eq!(from_usize.index(), 42);
    }

    #[test]
    fn test_debug_display_format() {
        let id = FileId::new(42);
        assert_eq!(format!("{id:?}"), "FileId(42)");
        assert_eq!(format!("{id}"), "file:42");

        assert_eq!(format!("{:?}", FileId::INVALID), "FileId(INVALID)");
        assert_eq!(format!("{}", FileId::INVALID), "INVALID");
    }

    #[test]
    fn test_serde_roundtrip() {
        let original = FileId::new(123);

        // JSON roundtrip
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: FileId = serde_json::from_str(&json).unwrap();
        assert_eq!(original, deserialized);

        // Postcard roundtrip
        let bytes = postcard::to_allocvec(&original).unwrap();
        let deserialized: FileId = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_size_of_file_id() {
        // Verify memory layout: u32 = 4 bytes
        assert_eq!(std::mem::size_of::<FileId>(), 4);
    }

    #[test]
    #[allow(clippy::clone_on_copy)] // Intentionally testing Clone trait
    fn test_copy_clone() {
        let id = FileId::new(10);
        let copied = id;
        let cloned = id.clone();

        assert_eq!(id, copied);
        assert_eq!(id, cloned);
    }
}
