//! `StringInterner`: Node name deduplication with reference counting.
//!
//! This module implements `StringInterner`, which provides efficient storage
//! of strings by deduplicating identical values.
//!
//! # Design (FR-3, FR-41)
//!
//! - **Deduplication**: Same string returns same `StringId`
//! - **Reference counting**: Tracks usage for GC eligibility
//! - **Thread-safe**: Uses `Arc<str>` for shared ownership
//!
//! # Memory Layout
//!
//! ```text
//! StringInterner:
//! ┌─────────────────────────────────────────────┐
//! │ strings: Vec<Arc<str>>                      │  Indexed by StringId
//! │ lookup: HashMap<Arc<str>, u32>              │  String → index
//! │ ref_counts: Vec<u32>                        │  Reference counts
//! │ free_list: Vec<u32>                         │  Recycled slots
//! └─────────────────────────────────────────────┘
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::string::id::StringId;

/// Error returned when a `StringId` is invalid or unresolvable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    /// The invalid `StringId`
    pub id: StringId,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to resolve StringId {:?}", self.id)
    }
}

impl std::error::Error for ResolveError {}

/// Error returned when interning a string fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InternError {
    /// The interner has exhausted all available IDs (> 2^32 - 2 strings).
    CapacityExhausted,
}

impl fmt::Display for InternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CapacityExhausted => {
                write!(f, "string interner capacity exhausted (> 2^32 - 2 strings)")
            }
        }
    }
}

impl std::error::Error for InternError {}

/// String interner for symbol name deduplication.
///
/// `StringInterner` stores strings efficiently by maintaining a single
/// copy of each unique string. When the same string is interned multiple
/// times, the same `StringId` is returned.
///
/// # Reference Counting
///
/// Each interned string has an associated reference count. This enables
/// garbage collection of unused strings during compaction phases.
///
/// # Thread Safety
///
/// The interner uses `Arc<str>` for string storage, making it safe to
/// share resolved strings across threads. However, the interner itself
/// requires external synchronization (e.g., `RwLock`) for concurrent access.
///
/// # Example
///
/// ```rust,ignore
/// let mut interner = StringInterner::new();
///
/// let id1 = interner.intern("foo");
/// let id2 = interner.intern("foo");
/// assert_eq!(id1, id2); // Same string → same ID
///
/// let resolved = interner.resolve(id1).unwrap();
/// assert_eq!(&*resolved, "foo");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringInterner {
    /// Storage of interned strings, indexed by `StringId`.
    strings: Vec<Option<Arc<str>>>,
    /// Reverse lookup from string to index.
    lookup: HashMap<Arc<str>, u32>,
    /// Reference count for each string.
    ref_counts: Vec<u32>,
    /// Free list of recycled slot indices.
    free_list: Vec<u32>,
    /// Optional maximum number of IDs (for testing error paths).
    ///
    /// When set, the interner will return `InternError::CapacityExhausted`
    /// when trying to allocate more than `max_ids` strings.
    /// When `None`, uses the default limit of `u32::MAX - 1`.
    #[serde(skip, default)]
    max_ids: Option<u32>,
}

impl StringInterner {
    /// Creates a new empty string interner.
    #[must_use]
    pub fn new() -> Self {
        // Reserve index 0 for INVALID sentinel
        Self {
            strings: vec![None],
            lookup: HashMap::new(),
            ref_counts: vec![0],
            free_list: Vec::new(),
            max_ids: None,
        }
    }

    /// Creates a new interner with the specified capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut strings = Vec::with_capacity(capacity + 1);
        let mut ref_counts = Vec::with_capacity(capacity + 1);
        strings.push(None); // Reserve index 0
        ref_counts.push(0);

        Self {
            strings,
            lookup: HashMap::with_capacity(capacity),
            ref_counts,
            free_list: Vec::new(),
            max_ids: None,
        }
    }

    /// Creates a new interner with a hard limit on the number of IDs.
    ///
    /// This constructor is designed for **testing error paths**. It allows
    /// deterministic testing of `InternError::CapacityExhausted` handling
    /// without requiring billions of strings.
    ///
    /// # Arguments
    ///
    /// * `max_ids` - Maximum number of unique strings that can be interned.
    ///   Once this limit is reached, `intern()` will return
    ///   `InternError::CapacityExhausted`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Create an interner that can only hold 3 strings
    /// let mut interner = StringInterner::with_max_ids(3);
    ///
    /// interner.intern("a").unwrap(); // OK
    /// interner.intern("b").unwrap(); // OK
    /// interner.intern("c").unwrap(); // OK
    /// assert!(interner.intern("d").is_err()); // CapacityExhausted
    /// ```
    #[must_use]
    pub fn with_max_ids(max_ids: u32) -> Self {
        Self {
            strings: vec![None],
            lookup: HashMap::new(),
            ref_counts: vec![0],
            free_list: Vec::new(),
            max_ids: Some(max_ids),
        }
    }

    /// Returns the number of interned strings (excluding INVALID slot).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    /// Returns true if no strings are interned.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }

    /// Interns a string and returns its `StringId`.
    ///
    /// If the string was already interned, returns the existing ID and
    /// increments its reference count. Otherwise, allocates a new ID.
    ///
    /// # Errors
    ///
    /// Returns `InternError::CapacityExhausted` if the interner has
    /// exhausted all available IDs (> 2^32 - 2 strings), or if `max_ids`
    /// is set and the limit has been reached.
    pub fn intern(&mut self, s: &str) -> Result<StringId, InternError> {
        // Check if already interned
        if let Some(&index) = self.lookup.get(s) {
            // Increment reference count
            self.ref_counts[index as usize] = self.ref_counts[index as usize].saturating_add(1);
            return Ok(StringId::new(index));
        }

        // Check max_ids limit if set (used for testing)
        if let Some(max) = self.max_ids
            && self.lookup.len() >= max as usize
        {
            return Err(InternError::CapacityExhausted);
        }

        // Allocate new slot
        let arc_str: Arc<str> = Arc::from(s);
        let index = if let Some(free_idx) = self.free_list.pop() {
            // Reuse a recycled slot
            self.strings[free_idx as usize] = Some(Arc::clone(&arc_str));
            self.ref_counts[free_idx as usize] = 1;
            free_idx
        } else {
            // Append new slot
            let idx = self.strings.len();
            let idx_u32 = u32::try_from(idx).map_err(|_| InternError::CapacityExhausted)?;
            if idx_u32 == u32::MAX {
                return Err(InternError::CapacityExhausted);
            }
            // Reserve the high bit for staging-local IDs.
            if idx_u32 & StringId::LOCAL_TAG_BIT != 0 {
                return Err(InternError::CapacityExhausted);
            }
            self.strings.push(Some(Arc::clone(&arc_str)));
            self.ref_counts.push(1);
            idx_u32
        };

        self.lookup.insert(arc_str, index);
        Ok(StringId::new(index))
    }

    /// Interns a string and returns its `StringId` without incrementing ref count.
    ///
    /// This is useful when the string is being stored in a structure that
    /// will manage its own lifetime (e.g., node entry).
    ///
    /// # Errors
    ///
    /// Returns `InternError::CapacityExhausted` if the interner has
    /// exhausted all available IDs (> 2^32 - 2 strings), or if `max_ids`
    /// is set and the limit has been reached.
    #[allow(dead_code)] // Used by graph builder (Step 20)
    pub fn intern_without_ref(&mut self, s: &str) -> Result<StringId, InternError> {
        // Check if already interned
        if let Some(&index) = self.lookup.get(s) {
            return Ok(StringId::new(index));
        }

        // Check max_ids limit if set (used for testing)
        if let Some(max) = self.max_ids
            && self.lookup.len() >= max as usize
        {
            return Err(InternError::CapacityExhausted);
        }

        // Allocate new slot with ref_count = 0
        let arc_str: Arc<str> = Arc::from(s);
        let index = if let Some(free_idx) = self.free_list.pop() {
            self.strings[free_idx as usize] = Some(Arc::clone(&arc_str));
            self.ref_counts[free_idx as usize] = 0;
            free_idx
        } else {
            let idx = self.strings.len();
            let idx_u32 = u32::try_from(idx).map_err(|_| InternError::CapacityExhausted)?;
            if idx_u32 == u32::MAX {
                return Err(InternError::CapacityExhausted);
            }
            // Reserve the high bit for staging-local IDs.
            if idx_u32 & StringId::LOCAL_TAG_BIT != 0 {
                return Err(InternError::CapacityExhausted);
            }
            self.strings.push(Some(Arc::clone(&arc_str)));
            self.ref_counts.push(0);
            idx_u32
        };

        self.lookup.insert(arc_str, index);
        Ok(StringId::new(index))
    }

    /// Resolves a `StringId` to its string value.
    ///
    /// Returns `None` if the ID is invalid or has been recycled.
    #[must_use]
    pub fn resolve(&self, id: StringId) -> Option<Arc<str>> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        self.strings.get(index).and_then(std::clone::Clone::clone)
    }

    /// Returns the reference count for a string.
    ///
    /// Returns 0 if the ID is invalid or has been recycled.
    #[must_use]
    pub fn ref_count(&self, id: StringId) -> u32 {
        if id.is_invalid() {
            return 0;
        }

        let index = id.index() as usize;
        self.ref_counts.get(index).copied().unwrap_or(0)
    }

    /// Increments the reference count for a string.
    ///
    /// Returns the new count, or None if the ID is invalid.
    pub fn inc_ref(&mut self, id: StringId) -> Option<u32> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        if index < self.ref_counts.len() && self.strings[index].is_some() {
            self.ref_counts[index] = self.ref_counts[index].saturating_add(1);
            Some(self.ref_counts[index])
        } else {
            None
        }
    }

    /// Decrements the reference count for a string.
    ///
    /// Returns the new count, or None if the ID is invalid.
    /// Note: This does NOT automatically recycle the string when count reaches 0.
    /// Use `recycle_unreferenced()` during compaction for that.
    pub fn dec_ref(&mut self, id: StringId) -> Option<u32> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        if index < self.ref_counts.len() && self.strings[index].is_some() {
            self.ref_counts[index] = self.ref_counts[index].saturating_sub(1);
            Some(self.ref_counts[index])
        } else {
            None
        }
    }

    /// Recycles all strings with zero reference count.
    ///
    /// Returns the number of strings recycled.
    /// This should be called during compaction phases.
    #[allow(dead_code)] // Used by Compaction (Step 15)
    pub fn recycle_unreferenced(&mut self) -> usize {
        let mut recycled = 0;

        for index in 1..self.strings.len() {
            if self.ref_counts[index] == 0
                && let Some(arc_str) = self.strings[index].take()
            {
                self.lookup.remove(&arc_str);
                if let Ok(index_u32) = u32::try_from(index) {
                    self.free_list.push(index_u32);
                }
                recycled += 1;
            }
        }

        recycled
    }

    /// Checks if a string is interned.
    #[must_use]
    pub fn contains(&self, s: &str) -> bool {
        self.lookup.contains_key(s)
    }

    /// Gets the `StringId` for a string if it's already interned.
    ///
    /// Unlike `intern()`, this does not create a new entry or modify ref counts.
    #[must_use]
    pub fn get(&self, s: &str) -> Option<StringId> {
        self.lookup.get(s).map(|&idx| StringId::new(idx))
    }

    /// Returns an iterator over all interned strings with their IDs.
    pub fn iter(&self) -> impl Iterator<Item = (StringId, &Arc<str>)> {
        self.strings
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                let index_u32 = u32::try_from(idx).ok()?;
                opt.as_ref().map(|s| (StringId::new(index_u32), s))
            })
    }

    /// Clears all interned strings.
    pub fn clear(&mut self) {
        self.strings.truncate(1); // Keep INVALID slot
        self.strings[0] = None;
        self.lookup.clear();
        self.ref_counts.truncate(1);
        self.ref_counts[0] = 0;
        self.free_list.clear();
    }

    /// Reserves capacity for at least `additional` more strings.
    pub fn reserve(&mut self, additional: usize) {
        self.strings.reserve(additional);
        self.ref_counts.reserve(additional);
        self.lookup.reserve(additional);
    }

    /// Returns statistics about the interner.
    #[must_use]
    pub fn stats(&self) -> InternerStats {
        let total_bytes: usize = self
            .strings
            .iter()
            .filter_map(|opt| opt.as_ref())
            .map(|s| s.len())
            .sum();

        InternerStats {
            string_count: self.len(),
            total_bytes,
            free_slots: self.free_list.len(),
            capacity: self.strings.capacity(),
        }
    }
}

impl Default for StringInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for StringInterner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "StringInterner(strings={}, free={})",
            self.len(),
            self.free_list.len()
        )
    }
}

/// Statistics about a `StringInterner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InternerStats {
    /// Number of interned strings.
    pub string_count: usize,
    /// Total bytes of string data.
    pub total_bytes: usize,
    /// Number of free (recyclable) slots.
    pub free_slots: usize,
    /// Allocated capacity.
    pub capacity: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let interner = StringInterner::new();
        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());
    }

    #[test]
    fn test_with_capacity() {
        let interner = StringInterner::with_capacity(100);
        assert_eq!(interner.len(), 0);
        assert!(interner.strings.capacity() >= 101); // +1 for INVALID slot
    }

    #[test]
    fn test_intern_single() {
        let mut interner = StringInterner::new();
        let id = interner.intern("hello").unwrap();

        assert!(!id.is_invalid());
        assert_eq!(interner.len(), 1);

        let resolved = interner.resolve(id).unwrap();
        assert_eq!(&*resolved, "hello");
    }

    #[test]
    fn test_intern_duplicate() {
        let mut interner = StringInterner::new();
        let id1 = interner.intern("foo").unwrap();
        let id2 = interner.intern("foo").unwrap();

        assert_eq!(id1, id2);
        assert_eq!(interner.len(), 1);
        assert_eq!(interner.ref_count(id1), 2);
    }

    #[test]
    fn test_intern_different() {
        let mut interner = StringInterner::new();
        let id1 = interner.intern("foo").unwrap();
        let id2 = interner.intern("bar").unwrap();

        assert_ne!(id1, id2);
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn test_resolve_invalid() {
        let interner = StringInterner::new();
        assert!(interner.resolve(StringId::INVALID).is_none());
    }

    #[test]
    fn test_resolve_out_of_bounds() {
        let interner = StringInterner::new();
        assert!(interner.resolve(StringId::new(999)).is_none());
    }

    #[test]
    fn test_ref_count() {
        let mut interner = StringInterner::new();
        let id = interner.intern("test").unwrap();

        assert_eq!(interner.ref_count(id), 1);

        interner.intern("test").unwrap(); // Same string
        assert_eq!(interner.ref_count(id), 2);

        interner.dec_ref(id);
        assert_eq!(interner.ref_count(id), 1);

        interner.inc_ref(id);
        assert_eq!(interner.ref_count(id), 2);
    }

    #[test]
    fn test_inc_ref_invalid() {
        let mut interner = StringInterner::new();
        assert!(interner.inc_ref(StringId::INVALID).is_none());
    }

    #[test]
    fn test_dec_ref_invalid() {
        let mut interner = StringInterner::new();
        assert!(interner.dec_ref(StringId::INVALID).is_none());
    }

    #[test]
    fn test_dec_ref_saturating() {
        let mut interner = StringInterner::new();
        let id = interner.intern_without_ref("test").unwrap();

        assert_eq!(interner.ref_count(id), 0);
        interner.dec_ref(id);
        assert_eq!(interner.ref_count(id), 0); // Saturates at 0
    }

    #[test]
    fn test_intern_without_ref() {
        let mut interner = StringInterner::new();
        let id = interner.intern_without_ref("test").unwrap();

        assert_eq!(interner.ref_count(id), 0);
        assert_eq!(interner.resolve(id).unwrap().as_ref(), "test");
    }

    #[test]
    fn test_recycle_unreferenced() {
        let mut interner = StringInterner::new();
        let id1 = interner.intern_without_ref("foo").unwrap();
        let id2 = interner.intern("bar").unwrap();

        assert_eq!(interner.len(), 2);

        let recycled = interner.recycle_unreferenced();
        assert_eq!(recycled, 1); // Only "foo" (ref_count = 0)
        assert_eq!(interner.len(), 1);

        // id1 should now be invalid
        assert!(interner.resolve(id1).is_none());
        // id2 should still work
        assert_eq!(interner.resolve(id2).unwrap().as_ref(), "bar");
    }

    #[test]
    fn test_free_list_reuse() {
        let mut interner = StringInterner::new();
        let id1 = interner.intern_without_ref("foo").unwrap();
        let _id2 = interner.intern("bar").unwrap();

        // Recycle "foo"
        interner.recycle_unreferenced();

        // New string should reuse the slot
        let id3 = interner.intern("baz").unwrap();
        assert_eq!(id3.index(), id1.index()); // Same slot reused
    }

    #[test]
    fn test_contains() {
        let mut interner = StringInterner::new();
        interner.intern("hello").unwrap();

        assert!(interner.contains("hello"));
        assert!(!interner.contains("world"));
    }

    #[test]
    fn test_get() {
        let mut interner = StringInterner::new();
        let id = interner.intern("hello").unwrap();

        assert_eq!(interner.get("hello"), Some(id));
        assert_eq!(interner.get("world"), None);
    }

    #[test]
    fn test_iter() {
        let mut interner = StringInterner::new();
        interner.intern("foo").unwrap();
        interner.intern("bar").unwrap();
        interner.intern("baz").unwrap();

        let strings: Vec<_> = interner.iter().map(|(_, s)| s.as_ref()).collect();
        assert_eq!(strings.len(), 3);
        assert!(strings.contains(&"foo"));
        assert!(strings.contains(&"bar"));
        assert!(strings.contains(&"baz"));
    }

    #[test]
    fn test_clear() {
        let mut interner = StringInterner::new();
        interner.intern("foo").unwrap();
        interner.intern("bar").unwrap();

        assert_eq!(interner.len(), 2);
        interner.clear();
        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());
    }

    #[test]
    fn test_reserve() {
        let mut interner = StringInterner::new();
        interner.reserve(1000);
        assert!(interner.strings.capacity() >= 1001);
    }

    #[test]
    fn test_display() {
        let mut interner = StringInterner::new();
        interner.intern("test").unwrap();

        let display = format!("{interner}");
        assert!(display.contains("StringInterner"));
        assert!(display.contains("strings=1"));
    }

    #[test]
    fn test_stats() {
        let mut interner = StringInterner::new();
        interner.intern("hello").unwrap(); // 5 bytes
        interner.intern("world").unwrap(); // 5 bytes

        let stats = interner.stats();
        assert_eq!(stats.string_count, 2);
        assert_eq!(stats.total_bytes, 10);
        assert_eq!(stats.free_slots, 0);
    }

    #[test]
    fn test_empty_string() {
        let mut interner = StringInterner::new();
        let id = interner.intern("").unwrap();

        assert!(!id.is_invalid());
        assert_eq!(interner.resolve(id).unwrap().as_ref(), "");
    }

    #[test]
    fn test_unicode() {
        let mut interner = StringInterner::new();
        let id = interner.intern("日本語").unwrap();

        let resolved = interner.resolve(id).unwrap();
        assert_eq!(&*resolved, "日本語");
    }

    #[test]
    fn test_default() {
        let interner: StringInterner = StringInterner::default();
        assert_eq!(interner.len(), 0);
    }

    #[test]
    fn test_resolve_error_display() {
        let err = ResolveError {
            id: StringId::new(42),
        };
        let display = format!("{err}");
        assert!(display.contains("StringId"));
    }

    #[test]
    fn test_intern_error_display() {
        let err = InternError::CapacityExhausted;
        let display = format!("{err}");
        assert!(display.contains("capacity exhausted"));
    }
}
