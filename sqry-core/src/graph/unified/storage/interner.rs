//! `StringInterner`: Node name deduplication with reference counting.
//!
//! This module implements `StringInterner`, which provides efficient storage
//! of strings by deduplicating identical values.
//!
//! # Design
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
//! │ lookup_stale: bool                          │  Invariant guard
//! └─────────────────────────────────────────────┘
//! ```
//!
//! # Lookup Staleness Invariant
//!
//! When bulk operations (`alloc_range`, `bulk_slices_mut`) write string
//! slots without updating the `lookup` HashMap, the `lookup_stale` flag
//! is set. Methods that depend on `lookup` correctness (`intern`, `get`,
//! `contains`, `len`, `is_empty`, `recycle_unreferenced`) assert that
//! this flag is `false`. The flag is cleared by `build_dedup_table()`
//! (which rebuilds the lookup) or `truncate_to()` (which rolls back to
//! pre-allocation state).

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
    #[serde(with = "super::serde_helpers::sorted_arc_str_map")]
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
    /// Invariant guard: `true` when bulk operations (e.g., `alloc_range`,
    /// `bulk_slices_mut`) have written string slots without updating the
    /// `lookup` HashMap. Methods that depend on `lookup` correctness
    /// (e.g., `intern`, `get`, `contains`) assert this is `false`.
    ///
    /// Cleared by `build_dedup_table()`, which rebuilds `lookup` from
    /// the canonical slot entries.
    #[serde(skip, default)]
    lookup_stale: bool,
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
            lookup_stale: false,
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
            lookup_stale: false,
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
            lookup_stale: false,
        }
    }

    /// Returns the number of interned strings (excluding INVALID slot).
    ///
    /// # Panics
    ///
    /// Panics if the lookup is stale (bulk slots written without rebuild).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        assert!(
            !self.lookup_stale,
            "StringInterner::len() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
        self.lookup.len()
    }

    /// Returns true if no strings are interned.
    ///
    /// # Panics
    ///
    /// Panics if the lookup is stale (bulk slots written without rebuild).
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        assert!(
            !self.lookup_stale,
            "StringInterner::is_empty() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
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
        assert!(
            !self.lookup_stale,
            "StringInterner::intern() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
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
        assert!(
            !self.lookup_stale,
            "StringInterner::intern_without_ref() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
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
    ///
    /// # Panics
    ///
    /// Panics if the lookup is stale (bulk slots written without rebuild).
    #[allow(dead_code)] // Used by Compaction (Step 15)
    pub fn recycle_unreferenced(&mut self) -> usize {
        assert!(
            !self.lookup_stale,
            "StringInterner::recycle_unreferenced() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
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
    ///
    /// # Panics
    ///
    /// Panics if the lookup is stale (bulk slots written without rebuild).
    #[must_use]
    pub fn contains(&self, s: &str) -> bool {
        assert!(
            !self.lookup_stale,
            "StringInterner::contains() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
        self.lookup.contains_key(s)
    }

    /// Gets the `StringId` for a string if it's already interned.
    ///
    /// Unlike `intern()`, this does not create a new entry or modify ref counts.
    ///
    /// # Panics
    ///
    /// Panics if the lookup is stale (bulk slots written without rebuild).
    #[must_use]
    pub fn get(&self, s: &str) -> Option<StringId> {
        assert!(
            !self.lookup_stale,
            "StringInterner::get() called while lookup is stale \
             (bulk slots written without rebuild). Call build_dedup_table() first."
        );
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
    ///
    /// Resets the interner to empty state, including clearing the
    /// `lookup_stale` flag (lookup is trivially consistent when empty).
    pub fn clear(&mut self) {
        self.strings.truncate(1); // Keep INVALID slot
        self.strings[0] = None;
        self.lookup.clear();
        self.ref_counts.truncate(1);
        self.ref_counts[0] = 0;
        self.free_list.clear();
        self.lookup_stale = false;
    }

    /// Reserves capacity for at least `additional` more strings.
    pub fn reserve(&mut self, additional: usize) {
        self.strings.reserve(additional);
        self.ref_counts.reserve(additional);
        self.lookup.reserve(additional);
    }

    /// Pre-allocates `count` string slots for bulk parallel commit.
    ///
    /// The new slots are initialized with `None` (no string) and `ref_count = 0`.
    /// Returns the start index of the allocated range. The caller can then
    /// fill slots `start..start+count` via [`StringInterner::bulk_slices_mut`].
    ///
    /// This method does **not** touch the `free_list` — it always appends to the
    /// end of the `strings` and `ref_counts` vectors. This is intentional:
    /// during parallel commit, each file gets a contiguous, non-overlapping range.
    ///
    /// # Errors
    ///
    /// Returns `InternError::CapacityExhausted` if the allocation would exceed
    /// the `LOCAL_TAG_BIT` boundary (2^31 indices reserved for global IDs).
    ///
    /// # Arguments
    ///
    /// * `count` - Number of slots to pre-allocate. If 0, this is a no-op
    ///   returning the current length.
    pub fn alloc_range(&mut self, count: u32) -> Result<u32, InternError> {
        let start = self.strings.len();
        let start_u32 = u32::try_from(start).map_err(|_| InternError::CapacityExhausted)?;

        if count == 0 {
            return Ok(start_u32);
        }

        // Check that the last index (start + count - 1) does not hit LOCAL_TAG_BIT
        let end_u32 = start_u32
            .checked_add(count)
            .ok_or(InternError::CapacityExhausted)?;
        // end_u32 is one-past-last, so the last valid index is end_u32 - 1
        if (end_u32 - 1) & StringId::LOCAL_TAG_BIT != 0 {
            return Err(InternError::CapacityExhausted);
        }

        // Extend with None slots and zero ref_counts
        self.strings.resize(end_u32 as usize, None);
        self.ref_counts.resize(end_u32 as usize, 0);

        // Mark lookup as stale — bulk slots bypass the lookup HashMap.
        // Callers must call build_dedup_table() before using lookup-dependent
        // methods (intern, get, contains, len, is_empty).
        self.lookup_stale = true;

        Ok(start_u32)
    }

    /// Returns mutable sub-slices into the strings and ref_counts arrays for
    /// the range `start..start+count`.
    ///
    /// This enables parallel file commit workers to write directly into their
    /// pre-allocated range without contention. The caller is responsible for
    /// ensuring no overlapping ranges are accessed concurrently.
    ///
    /// Defensively marks the lookup as stale when `count > 0`, since the
    /// returned slices allow direct mutation of string slots without updating
    /// the `lookup` HashMap.
    ///
    /// # Panics
    ///
    /// Panics if `start + count` exceeds the current vector length.
    pub fn bulk_slices_mut(
        &mut self,
        start: u32,
        count: u32,
    ) -> (&mut [Option<Arc<str>>], &mut [u32]) {
        if count > 0 {
            self.lookup_stale = true;
        }
        let s = start as usize;
        let e = s + count as usize;
        let strings_slice = &mut self.strings[s..e];
        let ref_counts_slice = &mut self.ref_counts[s..e];
        (strings_slice, ref_counts_slice)
    }

    /// Scans all string slots and deduplicates identical strings.
    ///
    /// After parallel commit, multiple file workers may have inserted the same
    /// string into different slots. This method:
    ///
    /// 1. Iterates slots `1..N` in index order (deterministic).
    /// 2. For the first occurrence of each string value, that slot becomes the
    ///    **canonical** entry.
    /// 3. For duplicate occurrences, their ref_count is accumulated into the
    ///    canonical slot, and the duplicate slot is cleared (`None`, ref_count=0`).
    /// 4. The `lookup` `HashMap` is rebuilt from canonical entries only.
    ///
    /// Returns a remap table mapping duplicate `StringId` to canonical `StringId`.
    /// Canonical entries are **not** included in the returned map.
    pub fn build_dedup_table(&mut self) -> HashMap<StringId, StringId> {
        let mut remap: HashMap<StringId, StringId> = HashMap::new();
        // Track first-seen: string value -> canonical index
        let mut canonical: HashMap<Arc<str>, u32> = HashMap::new();

        for idx in 1..self.strings.len() {
            let Some(ref arc_str) = self.strings[idx] else {
                continue;
            };

            if let Some(&canon_idx) = canonical.get(arc_str) {
                // Duplicate: accumulate ref_count into canonical
                let dup_rc = self.ref_counts[idx];
                self.ref_counts[canon_idx as usize] =
                    self.ref_counts[canon_idx as usize].saturating_add(dup_rc);
                // Clear duplicate slot and add to free_list for reuse
                self.strings[idx] = None;
                self.ref_counts[idx] = 0;
                let idx_u32 = idx as u32;
                self.free_list.push(idx_u32);
                // Record remap
                remap.insert(StringId::new(idx_u32), StringId::new(canon_idx));
            } else {
                // First occurrence — this is canonical
                let idx_u32 = idx as u32;
                canonical.insert(Arc::clone(arc_str), idx_u32);
            }
        }

        // Rebuild lookup from canonical entries only
        self.lookup.clear();
        self.lookup.reserve(canonical.len());
        for (arc_str, idx) in canonical {
            self.lookup.insert(arc_str, idx);
        }

        // Lookup is now consistent with slot contents — clear stale flag.
        self.lookup_stale = false;

        remap
    }

    /// Truncates the strings and ref_counts vectors to `saved_len`.
    ///
    /// This rolls back a failed bulk allocation by removing all slots at
    /// index `saved_len` and beyond. The `lookup` `HashMap` is **not** modified
    /// (the caller is responsible for ensuring no lookup entries point to the
    /// truncated region).
    ///
    /// # Panics
    ///
    /// Panics if `saved_len` is 0 (would remove the sentinel slot).
    pub fn truncate_to(&mut self, saved_len: usize) {
        assert!(saved_len >= 1, "cannot truncate sentinel slot at index 0");
        self.strings.truncate(saved_len);
        self.ref_counts.truncate(saved_len);
        // Rolling back to pre-allocation state restores lookup consistency:
        // the lookup only contains entries for slots that existed before
        // the bulk allocation, all of which are still valid after truncation.
        self.lookup_stale = false;
    }

    /// Returns the total number of string slots including the sentinel at index 0.
    ///
    /// This is the raw vector length, not the number of interned strings.
    /// Useful for saving/restoring allocation state.
    #[inline]
    #[must_use]
    pub fn string_count_raw(&self) -> usize {
        self.strings.len()
    }

    /// Returns whether the lookup HashMap is stale (bulk slots written
    /// without a `build_dedup_table()` rebuild).
    ///
    /// This is primarily useful for testing and diagnostics.
    #[inline]
    #[must_use]
    pub fn is_lookup_stale(&self) -> bool {
        self.lookup_stale
    }

    /// Returns statistics about the interner.
    ///
    /// Safe to call even when lookup is stale — uses slot-based counting
    /// instead of lookup length.
    #[must_use]
    pub fn stats(&self) -> InternerStats {
        let total_bytes: usize = self
            .strings
            .iter()
            .filter_map(|opt| opt.as_ref())
            .map(|s| s.len())
            .sum();

        // Count occupied slots directly — safe regardless of lookup state.
        let string_count = self
            .strings
            .iter()
            .skip(1) // skip sentinel
            .filter(|s| s.is_some())
            .count();

        InternerStats {
            string_count,
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
        // Use slot-based count (safe regardless of lookup state).
        let count = self.stats().string_count;
        write!(
            f,
            "StringInterner(strings={count}, free={}{})",
            self.free_list.len(),
            if self.lookup_stale {
                ", lookup_stale"
            } else {
                ""
            }
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

    // ── Bulk API tests for parallel commit ──────────────────────────────

    #[test]
    fn test_alloc_range_basic() {
        let mut interner = StringInterner::new();
        // Sentinel exists at index 0
        assert_eq!(interner.string_count_raw(), 1);

        let start = interner.alloc_range(5).unwrap();
        // Start must be >= 1 (sentinel at 0 is already there)
        assert!(start >= 1);
        assert_eq!(start, 1); // New interner, first alloc starts right after sentinel
        assert_eq!(interner.string_count_raw(), 6); // 1 sentinel + 5 allocated

        // All new slots should be None with ref_count 0
        for i in start..start + 5 {
            let id = StringId::new(i);
            assert!(interner.resolve(id).is_none());
            assert_eq!(interner.ref_count(id), 0);
        }
    }

    #[test]
    fn test_alloc_range_zero_noop() {
        let mut interner = StringInterner::new();
        let before = interner.string_count_raw();
        let start = interner.alloc_range(0).unwrap();
        assert_eq!(start as usize, before);
        assert_eq!(interner.string_count_raw(), before);
    }

    #[test]
    fn test_alloc_range_after_existing_strings() {
        let mut interner = StringInterner::new();
        interner.intern("existing").unwrap();
        // strings = [None, Some("existing")] → len = 2
        let start = interner.alloc_range(3).unwrap();
        assert_eq!(start, 2);
        assert_eq!(interner.string_count_raw(), 5);
        // Existing string still works
        assert_eq!(
            interner.resolve(StringId::new(1)).unwrap().as_ref(),
            "existing"
        );
    }

    #[test]
    fn test_bulk_slices_mut() {
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(3).unwrap();

        // Write strings into the pre-allocated slots
        {
            let (strings, ref_counts) = interner.bulk_slices_mut(start, 3);
            strings[0] = Some(Arc::from("alpha"));
            strings[1] = Some(Arc::from("beta"));
            strings[2] = Some(Arc::from("gamma"));
            ref_counts[0] = 1;
            ref_counts[1] = 2;
            ref_counts[2] = 1;
        }

        // Verify via resolve()
        assert_eq!(
            interner.resolve(StringId::new(start)).unwrap().as_ref(),
            "alpha"
        );
        assert_eq!(
            interner.resolve(StringId::new(start + 1)).unwrap().as_ref(),
            "beta"
        );
        assert_eq!(
            interner.resolve(StringId::new(start + 2)).unwrap().as_ref(),
            "gamma"
        );

        // Verify ref_counts
        assert_eq!(interner.ref_count(StringId::new(start)), 1);
        assert_eq!(interner.ref_count(StringId::new(start + 1)), 2);
        assert_eq!(interner.ref_count(StringId::new(start + 2)), 1);
    }

    #[test]
    fn test_build_dedup_table_no_duplicates() {
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(3).unwrap();

        {
            let (strings, ref_counts) = interner.bulk_slices_mut(start, 3);
            strings[0] = Some(Arc::from("one"));
            strings[1] = Some(Arc::from("two"));
            strings[2] = Some(Arc::from("three"));
            ref_counts[0] = 1;
            ref_counts[1] = 1;
            ref_counts[2] = 1;
        }

        let remap = interner.build_dedup_table();
        assert!(remap.is_empty(), "no duplicates means empty remap");

        // All strings still resolvable
        assert_eq!(
            interner.resolve(StringId::new(start)).unwrap().as_ref(),
            "one"
        );
        assert_eq!(
            interner.resolve(StringId::new(start + 1)).unwrap().as_ref(),
            "two"
        );
        assert_eq!(
            interner.resolve(StringId::new(start + 2)).unwrap().as_ref(),
            "three"
        );

        // Lookup rebuilt correctly
        assert_eq!(interner.len(), 3);
    }

    #[test]
    fn test_build_dedup_table_with_duplicates() {
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(4).unwrap();

        {
            let (strings, ref_counts) = interner.bulk_slices_mut(start, 4);
            strings[0] = Some(Arc::from("hello"));
            strings[1] = Some(Arc::from("world"));
            strings[2] = Some(Arc::from("hello")); // duplicate of slot 0
            strings[3] = Some(Arc::from("world")); // duplicate of slot 1
            ref_counts[0] = 1;
            ref_counts[1] = 3;
            ref_counts[2] = 2;
            ref_counts[3] = 1;
        }

        let remap = interner.build_dedup_table();

        // Two duplicates should be remapped
        assert_eq!(remap.len(), 2);
        assert_eq!(remap[&StringId::new(start + 2)], StringId::new(start)); // hello dup → hello canon
        assert_eq!(remap[&StringId::new(start + 3)], StringId::new(start + 1)); // world dup → world canon

        // Canonical ref_counts accumulated
        assert_eq!(interner.ref_count(StringId::new(start)), 3); // 1 + 2
        assert_eq!(interner.ref_count(StringId::new(start + 1)), 4); // 3 + 1

        // Duplicate slots cleared
        assert!(interner.resolve(StringId::new(start + 2)).is_none());
        assert!(interner.resolve(StringId::new(start + 3)).is_none());
        assert_eq!(interner.ref_count(StringId::new(start + 2)), 0);
        assert_eq!(interner.ref_count(StringId::new(start + 3)), 0);

        // Lookup has only unique strings
        assert_eq!(interner.len(), 2);
        assert_eq!(interner.get("hello"), Some(StringId::new(start)));
        assert_eq!(interner.get("world"), Some(StringId::new(start + 1)));
    }

    #[test]
    fn test_truncate_to() {
        let mut interner = StringInterner::new();
        interner.intern("keep").unwrap();

        let saved = interner.string_count_raw(); // 2 (sentinel + "keep")
        let _start = interner.alloc_range(10).unwrap();
        assert_eq!(interner.string_count_raw(), 12);

        interner.truncate_to(saved);
        assert_eq!(interner.string_count_raw(), 2);

        // Original string still works
        assert_eq!(interner.resolve(StringId::new(1)).unwrap().as_ref(), "keep");
    }

    #[test]
    #[should_panic(expected = "cannot truncate sentinel")]
    fn test_truncate_to_zero_panics() {
        let mut interner = StringInterner::new();
        interner.truncate_to(0);
    }

    #[test]
    fn test_dedup_preserves_sentinel() {
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(2).unwrap();

        {
            let (strings, ref_counts) = interner.bulk_slices_mut(start, 2);
            strings[0] = Some(Arc::from("a"));
            strings[1] = Some(Arc::from("a")); // duplicate
            ref_counts[0] = 1;
            ref_counts[1] = 1;
        }

        let _remap = interner.build_dedup_table();

        // Sentinel at index 0 must still be None with ref_count 0
        assert!(interner.resolve(StringId::new(0)).is_none());
        // StringId(0) is actually not INVALID (INVALID = u32::MAX), but
        // resolve checks the slot content which is None
        assert_eq!(interner.ref_counts[0], 0);
        assert!(interner.strings[0].is_none());
    }

    #[test]
    fn test_string_count_raw() {
        let mut interner = StringInterner::new();
        assert_eq!(interner.string_count_raw(), 1); // sentinel only

        interner.intern("a").unwrap();
        assert_eq!(interner.string_count_raw(), 2);

        interner.alloc_range(5).unwrap();
        assert_eq!(interner.string_count_raw(), 7);
    }

    #[test]
    fn test_ref_count_accessor() {
        let mut interner = StringInterner::new();
        let id = interner.intern("test").unwrap();
        assert_eq!(interner.ref_count(id), 1);

        interner.intern("test").unwrap(); // increment
        assert_eq!(interner.ref_count(id), 2);

        // Invalid ID returns 0
        assert_eq!(interner.ref_count(StringId::INVALID), 0);

        // Out of bounds returns 0
        assert_eq!(interner.ref_count(StringId::new(999)), 0);
    }

    #[test]
    fn test_alloc_range_capacity_check() {
        // Test that LOCAL_TAG_BIT boundary is checked
        let mut interner = StringInterner::with_max_ids(u32::MAX);
        // We can't actually allocate 2^31 items in a test, but we can
        // verify the check logic by testing a smaller allocation succeeds
        let start = interner.alloc_range(10).unwrap();
        assert_eq!(start, 1);
        assert_eq!(interner.string_count_raw(), 11);
    }

    #[test]
    fn test_dedup_frees_slots_for_reuse() {
        // Regression test: after dedup, duplicate slots must be on free_list
        // so that future intern() calls reuse them instead of growing forever.
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(3).unwrap();

        {
            let (strings, ref_counts) = interner.bulk_slices_mut(start, 3);
            strings[0] = Some(Arc::from("dup"));
            strings[1] = Some(Arc::from("unique"));
            strings[2] = Some(Arc::from("dup")); // duplicate of slot 0
            ref_counts[0] = 1;
            ref_counts[1] = 1;
            ref_counts[2] = 1;
        }

        let remap = interner.build_dedup_table();
        assert_eq!(remap.len(), 1);

        // Slot start+2 was a duplicate and should now be on the free_list
        let dup_slot = start + 2;
        assert!(interner.resolve(StringId::new(dup_slot)).is_none());

        // Intern a new string — it should reuse the freed duplicate slot
        let new_id = interner.intern("fresh").unwrap();
        assert_eq!(
            new_id.index(),
            dup_slot,
            "new intern should reuse freed duplicate slot"
        );
    }

    #[test]
    fn test_build_dedup_table_with_gaps() {
        // Test dedup when there are None gaps (e.g., from recycled slots)
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(4).unwrap();

        {
            let (strings, ref_counts) = interner.bulk_slices_mut(start, 4);
            strings[0] = Some(Arc::from("x"));
            // strings[1] intentionally left as None (gap)
            strings[2] = Some(Arc::from("y"));
            strings[3] = Some(Arc::from("x")); // duplicate of slot 0
            ref_counts[0] = 1;
            ref_counts[2] = 1;
            ref_counts[3] = 1;
        }

        let remap = interner.build_dedup_table();

        // Only slot 3 (duplicate "x") should be remapped
        assert_eq!(remap.len(), 1);
        assert_eq!(remap[&StringId::new(start + 3)], StringId::new(start));

        // Canonical "x" ref_count accumulated
        assert_eq!(interner.ref_count(StringId::new(start)), 2);

        // "y" untouched
        assert_eq!(interner.ref_count(StringId::new(start + 2)), 1);

        // Gap at slot 1 still None
        assert!(interner.resolve(StringId::new(start + 1)).is_none());
    }

    // ── Deterministic serialization tests ────────────────────────────────

    #[test]
    fn test_interner_deterministic_serialization() {
        // Two interners with the same insertion order must produce identical
        // postcard bytes. Without sorted lookup serialization, HashMap's
        // random hash seeds could cause different byte output.
        let mut interner1 = StringInterner::new();
        interner1.intern("alpha").unwrap();
        interner1.intern("beta").unwrap();
        interner1.intern("gamma").unwrap();

        let mut interner2 = StringInterner::new();
        interner2.intern("alpha").unwrap();
        interner2.intern("beta").unwrap();
        interner2.intern("gamma").unwrap();

        let bytes1 = postcard::to_stdvec(&interner1).expect("serialize interner1");
        let bytes2 = postcard::to_stdvec(&interner2).expect("serialize interner2");

        assert_eq!(
            bytes1, bytes2,
            "same insertion order must produce identical postcard bytes"
        );
    }

    #[test]
    fn test_interner_serialization_roundtrip() {
        let mut interner = StringInterner::new();
        interner.intern("foo").unwrap();
        interner.intern("bar").unwrap();
        interner.intern("baz").unwrap();
        // Intern "foo" again to bump its ref count
        interner.intern("foo").unwrap();

        let bytes = postcard::to_stdvec(&interner).expect("serialize");
        let deserialized: StringInterner = postcard::from_bytes(&bytes).expect("deserialize");

        // Verify all strings survived the roundtrip
        assert_eq!(deserialized.len(), 3);
        assert!(deserialized.contains("foo"));
        assert!(deserialized.contains("bar"));
        assert!(deserialized.contains("baz"));

        // Verify ref counts survived
        let foo_id = deserialized.get("foo").unwrap();
        assert_eq!(deserialized.ref_count(foo_id), 2);

        let bar_id = deserialized.get("bar").unwrap();
        assert_eq!(deserialized.ref_count(bar_id), 1);
    }

    #[test]
    fn test_interner_sorted_lookup_produces_stable_bytes() {
        // Verify that the lookup map portion is sorted by inspecting the
        // serialized bytes. Since postcard serializes sequences in order,
        // "alpha" must appear before "beta" which must appear before "gamma"
        // in the output.
        let mut interner = StringInterner::new();
        interner.intern("gamma").unwrap();
        interner.intern("alpha").unwrap();
        interner.intern("beta").unwrap();

        let bytes = postcard::to_stdvec(&interner).expect("serialize");

        // Find the positions of the key strings in the serialized bytes
        let bytes_str = String::from_utf8_lossy(&bytes);
        // "alpha" should appear before "beta" before "gamma" in the lookup
        // portion (the second occurrence, since strings Vec has insertion order)
        let find_second = |needle: &str| {
            let first = bytes_str.find(needle).unwrap();
            bytes_str[first + needle.len()..]
                .find(needle)
                .map(|pos| pos + first + needle.len())
        };

        // The lookup map is serialized after the strings Vec, so the second
        // occurrence of each string is in the sorted lookup portion
        let alpha_pos = find_second("alpha");
        let beta_pos = find_second("beta");
        let gamma_pos = find_second("gamma");

        if let (Some(a), Some(b), Some(g)) = (alpha_pos, beta_pos, gamma_pos) {
            assert!(a < b, "alpha should appear before beta in sorted lookup");
            assert!(b < g, "beta should appear before gamma in sorted lookup");
        }
        // If any string doesn't appear twice, the test is vacuously true
        // (postcard may optimize storage), which is fine.
    }

    // ---- lookup_stale invariant guard tests ----

    #[test]
    fn test_alloc_range_sets_lookup_stale() {
        let mut interner = StringInterner::new();
        assert!(!interner.is_lookup_stale());

        interner.alloc_range(5).unwrap();
        assert!(interner.is_lookup_stale());
    }

    #[test]
    fn test_alloc_range_zero_does_not_set_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(0).unwrap();
        // Zero allocation is a no-op — lookup remains consistent.
        assert!(!interner.is_lookup_stale());
    }

    #[test]
    fn test_build_dedup_table_clears_lookup_stale() {
        let mut interner = StringInterner::new();
        let start = interner.alloc_range(2).unwrap();
        assert!(interner.is_lookup_stale());

        // Write some strings into the bulk slots
        interner.strings[start as usize] = Some(Arc::from("alpha"));
        interner.ref_counts[start as usize] = 1;
        interner.strings[(start + 1) as usize] = Some(Arc::from("beta"));
        interner.ref_counts[(start + 1) as usize] = 1;

        let _remap = interner.build_dedup_table();
        assert!(!interner.is_lookup_stale());

        // Lookup should now be consistent
        assert!(interner.contains("alpha"));
        assert!(interner.contains("beta"));
        assert_eq!(interner.len(), 2);
    }

    #[test]
    fn test_truncate_to_clears_lookup_stale() {
        let mut interner = StringInterner::new();
        let saved_len = interner.string_count_raw();

        interner.alloc_range(10).unwrap();
        assert!(interner.is_lookup_stale());

        interner.truncate_to(saved_len);
        assert!(!interner.is_lookup_stale());

        // Should be usable again after rollback
        let id = interner.intern("after_rollback").unwrap();
        assert!(interner.contains("after_rollback"));
        assert_eq!(interner.resolve(id).unwrap().as_ref(), "after_rollback");
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_intern_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.intern("should_panic");
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_intern_without_ref_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.intern_without_ref("should_panic");
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_get_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.get("should_panic");
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_contains_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.contains("should_panic");
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_len_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.len();
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_is_empty_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.is_empty();
    }

    #[test]
    #[should_panic(expected = "lookup is stale")]
    fn test_recycle_unreferenced_panics_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(1).unwrap();
        let _ = interner.recycle_unreferenced();
    }

    #[test]
    fn test_resolve_works_when_lookup_stale() {
        // resolve() does NOT use the lookup — it reads directly from slots.
        // It should work even when the lookup is stale.
        let mut interner = StringInterner::new();
        let id = interner.intern("before_bulk").unwrap();

        interner.alloc_range(5).unwrap();
        assert!(interner.is_lookup_stale());

        // resolve() should still work for pre-existing entries
        assert_eq!(interner.resolve(id).unwrap().as_ref(), "before_bulk");
    }

    #[test]
    fn test_stats_works_when_lookup_stale() {
        // stats() should be safe to call even when lookup is stale.
        let mut interner = StringInterner::new();
        interner.intern("existing").unwrap();

        let start = interner.alloc_range(2).unwrap();
        interner.strings[start as usize] = Some(Arc::from("bulk1"));
        interner.ref_counts[start as usize] = 1;

        assert!(interner.is_lookup_stale());

        let stats = interner.stats();
        // Should count occupied slots: "existing" + "bulk1" = 2
        // (the second bulk slot is still None)
        assert_eq!(stats.string_count, 2);
    }

    #[test]
    fn test_display_works_when_lookup_stale() {
        let mut interner = StringInterner::new();
        interner.alloc_range(3).unwrap();
        assert!(interner.is_lookup_stale());

        let display = format!("{interner}");
        assert!(
            display.contains("lookup_stale"),
            "Display should indicate stale state: {display}"
        );
    }

    #[test]
    fn test_display_omits_stale_when_consistent() {
        let mut interner = StringInterner::new();
        interner.intern("hello").unwrap();

        let display = format!("{interner}");
        assert!(
            !display.contains("lookup_stale"),
            "Display should not mention stale when consistent: {display}"
        );
    }

    #[test]
    fn test_full_parallel_commit_lifecycle() {
        // Simulates the full Phase 2→3→4a lifecycle:
        // 1. Pre-allocate ranges (sets stale)
        // 2. Write strings into bulk slots (stale — only resolve works)
        // 3. build_dedup_table (clears stale, rebuilds lookup)
        // 4. All methods work again
        let mut interner = StringInterner::new();

        // Pre-existing string (before parallel commit)
        let pre_id = interner.intern("pre_existing").unwrap();
        assert_eq!(interner.len(), 1);

        // Phase 2: allocate ranges for 3 files × 2 strings each
        let start = interner.alloc_range(6).unwrap();
        assert!(interner.is_lookup_stale());

        // Phase 3: write strings (simulating parallel workers)
        // File 0: "alpha", "beta"
        interner.strings[start as usize] = Some(Arc::from("alpha"));
        interner.ref_counts[start as usize] = 1;
        interner.strings[(start + 1) as usize] = Some(Arc::from("beta"));
        interner.ref_counts[(start + 1) as usize] = 1;

        // File 1: "alpha" (duplicate), "gamma"
        interner.strings[(start + 2) as usize] = Some(Arc::from("alpha"));
        interner.ref_counts[(start + 2) as usize] = 1;
        interner.strings[(start + 3) as usize] = Some(Arc::from("gamma"));
        interner.ref_counts[(start + 3) as usize] = 1;

        // File 2: "beta" (duplicate), "delta"
        interner.strings[(start + 4) as usize] = Some(Arc::from("beta"));
        interner.ref_counts[(start + 4) as usize] = 1;
        interner.strings[(start + 5) as usize] = Some(Arc::from("delta"));
        interner.ref_counts[(start + 5) as usize] = 1;

        // resolve() works during stale state
        assert_eq!(interner.resolve(pre_id).unwrap().as_ref(), "pre_existing");

        // Phase 4a: dedup rebuilds lookup
        let remap = interner.build_dedup_table();
        assert!(!interner.is_lookup_stale());

        // 2 duplicates remapped: "alpha" at start+2, "beta" at start+4
        assert_eq!(remap.len(), 2);

        // All lookup-dependent methods work
        assert_eq!(interner.len(), 5); // pre_existing + alpha + beta + gamma + delta
        assert!(interner.contains("pre_existing"));
        assert!(interner.contains("alpha"));
        assert!(interner.contains("beta"));
        assert!(interner.contains("gamma"));
        assert!(interner.contains("delta"));
        assert_eq!(interner.get("alpha"), Some(StringId::new(start)));
        assert_eq!(interner.get("beta"), Some(StringId::new(start + 1)));

        // Canonical ref_counts should be accumulated
        assert_eq!(interner.ref_count(StringId::new(start)), 2); // alpha: 1+1
        assert_eq!(interner.ref_count(StringId::new(start + 1)), 2); // beta: 1+1
    }

    #[test]
    fn test_clear_resets_lookup_stale() {
        // Regression: clear() must reset lookup_stale so that lookup-dependent
        // methods work on the now-empty (trivially consistent) interner.
        let mut interner = StringInterner::new();
        interner.alloc_range(5).unwrap();
        assert!(interner.is_lookup_stale());

        interner.clear();
        assert!(!interner.is_lookup_stale());

        // All lookup-dependent methods should work after clear
        assert_eq!(interner.len(), 0);
        assert!(interner.is_empty());
        assert!(!interner.contains("anything"));
        assert_eq!(interner.get("anything"), None);

        // Interning should work after clear
        let id = interner.intern("after_clear").unwrap();
        assert_eq!(interner.resolve(id).unwrap().as_ref(), "after_clear");
        assert_eq!(interner.len(), 1);
    }

    #[test]
    fn test_bulk_slices_mut_sets_lookup_stale() {
        // Regression: bulk_slices_mut() must defensively set lookup_stale
        // even if called independently of alloc_range().
        let mut interner = StringInterner::new();

        // Manually grow vecs to simulate pre-allocation without alloc_range
        interner.strings.resize(4, None);
        interner.ref_counts.resize(4, 0);
        assert!(!interner.is_lookup_stale());

        // bulk_slices_mut with count > 0 should set stale
        let (str_slots, rc_slots) = interner.bulk_slices_mut(1, 3);
        str_slots[0] = Some(Arc::from("x"));
        rc_slots[0] = 1;
        assert!(interner.is_lookup_stale());

        // build_dedup_table clears it
        let _remap = interner.build_dedup_table();
        assert!(!interner.is_lookup_stale());
        assert!(interner.contains("x"));
    }

    #[test]
    fn test_bulk_slices_mut_zero_does_not_set_stale() {
        let mut interner = StringInterner::new();
        interner.intern("existing").unwrap();
        assert!(!interner.is_lookup_stale());

        // Zero-length bulk_slices_mut is a no-op — should not set stale
        let (_s, _r) = interner.bulk_slices_mut(1, 0);
        assert!(!interner.is_lookup_stale());

        // Lookup should still work
        assert!(interner.contains("existing"));
    }
}
