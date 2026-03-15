//! `NodeArena`: Contiguous node storage with generational indices.
//!
//! This module implements `NodeArena`, the primary node storage for the
//! unified graph architecture. It provides:
//!
//! - **O(1) access**: Direct indexing into contiguous storage
//! - **Generational safety**: Stale IDs return `None` instead of wrong data
//! - **Free list reuse**: Deleted slots are recycled efficiently
//!
//! # Design
//!
//! The arena uses a slot-based design:
//! - Each slot is either occupied (with data) or vacant (with next-free pointer)
//! - Occupied slots store `NodeEntry` with generation counter
//! - Vacant slots form a singly-linked free list
//! - Generation counter increments on each reuse to detect stale IDs

use std::fmt;

use serde::{Deserialize, Serialize};

use super::super::file::id::FileId;
use super::super::node::id::{GenerationOverflowError, NodeId};
use super::super::node::kind::NodeKind;
use super::super::string::id::StringId;
use crate::graph::body_hash::BodyHash128;

/// Error type for arena allocation operations.
///
/// This enum covers all possible failure modes during node allocation,
/// providing graceful error handling instead of panics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArenaError {
    /// Generation counter would overflow, risking stale ID collision.
    GenerationOverflow(GenerationOverflowError),

    /// Free list is corrupted: an occupied slot was found in the free list.
    ///
    /// This indicates a serious internal consistency error, likely caused by
    /// concurrent modification without proper synchronization, memory corruption,
    /// or a bug in the arena implementation.
    FreeListCorruption {
        /// Index of the corrupted slot.
        index: u32,
    },

    /// Arena capacity exceeded: cannot allocate more than `u32::MAX` nodes.
    CapacityExceeded,
}

impl fmt::Display for ArenaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArenaError::GenerationOverflow(e) => write!(f, "{e}"),
            ArenaError::FreeListCorruption { index } => {
                write!(
                    f,
                    "free list corruption: occupied slot found at index {index}"
                )
            }
            ArenaError::CapacityExceeded => {
                write!(
                    f,
                    "arena capacity exceeded: cannot allocate more than {} nodes",
                    u32::MAX
                )
            }
        }
    }
}

impl std::error::Error for ArenaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ArenaError::GenerationOverflow(e) => Some(e),
            _ => None,
        }
    }
}

impl From<GenerationOverflowError> for ArenaError {
    fn from(e: GenerationOverflowError) -> Self {
        ArenaError::GenerationOverflow(e)
    }
}

/// State of a slot in the arena.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SlotState<T> {
    /// Slot contains an occupied entry.
    Occupied(T),
    /// Slot is vacant, contains next free index (or None if end of list).
    Vacant {
        /// Index of the next free slot in the free list, or None if end.
        next_free: Option<u32>,
    },
}

/// A slot in the arena with generation tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slot<T> {
    /// Current generation for this slot.
    generation: u64,
    /// Slot state (occupied or vacant).
    state: SlotState<T>,
}

impl<T> Slot<T> {
    /// Creates a new vacant slot with the given generation and next free.
    #[allow(dead_code)] // Used by Compaction (Step 15)
    fn new_vacant(generation: u64, next_free: Option<u32>) -> Self {
        Self {
            generation,
            state: SlotState::Vacant { next_free },
        }
    }

    /// Creates a new occupied slot with the given generation and data.
    pub(crate) fn new_occupied(generation: u64, data: T) -> Self {
        Self {
            generation,
            state: SlotState::Occupied(data),
        }
    }

    /// Returns true if this slot is occupied.
    #[inline]
    pub fn is_occupied(&self) -> bool {
        matches!(self.state, SlotState::Occupied(_))
    }

    /// Returns true if this slot is vacant.
    #[inline]
    pub fn is_vacant(&self) -> bool {
        matches!(self.state, SlotState::Vacant { .. })
    }

    /// Returns the generation of this slot.
    #[inline]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Returns a reference to the occupied data, if any.
    pub fn get(&self) -> Option<&T> {
        match &self.state {
            SlotState::Occupied(data) => Some(data),
            SlotState::Vacant { .. } => None,
        }
    }

    /// Returns a reference to the slot state (occupied or vacant).
    #[inline]
    pub fn state(&self) -> &SlotState<T> {
        &self.state
    }

    /// Returns a mutable reference to the occupied data, if any.
    pub fn get_mut(&mut self) -> Option<&mut T> {
        match &mut self.state {
            SlotState::Occupied(data) => Some(data),
            SlotState::Vacant { .. } => None,
        }
    }
}

/// A node entry stored in the arena.
///
/// This is the actual data stored for each node in the graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEntry {
    /// The kind of code entity this node represents.
    pub kind: NodeKind,
    /// Interned name of the symbol.
    pub name: StringId,
    /// File containing this node.
    pub file: FileId,
    /// Start byte offset in the file.
    pub start_byte: u32,
    /// End byte offset in the file.
    pub end_byte: u32,
    /// Start line (1-indexed).
    pub start_line: u32,
    /// Start column (0-indexed).
    pub start_column: u32,
    /// End line (1-indexed).
    pub end_line: u32,
    /// End column (0-indexed).
    pub end_column: u32,
    /// Optional signature/type information.
    pub signature: Option<StringId>,
    /// Optional docstring.
    pub doc: Option<StringId>,
    /// Optional qualified name (e.g., module.Class.method).
    pub qualified_name: Option<StringId>,
    /// Optional visibility modifier (public, private, protected, etc.).
    pub visibility: Option<StringId>,
    /// Whether this is an async function/method.
    pub is_async: bool,
    /// Whether this is a static member.
    pub is_static: bool,
    /// 128-bit body hash for duplicate detection.
    ///
    /// Computed from the raw body bytes of hashable symbol kinds:
    /// Function, Method, Class, Struct, Enum, Interface, Trait, Module.
    ///
    /// CRITICAL: This field MUST remain BEFORE `is_unsafe` for
    /// postcard serialization compatibility with older snapshots.
    /// Uses `#[serde(default)]` to read older V2 snapshots that lack this field.
    /// DO NOT use `skip_serializing_if` - postcard is positional and skipping corrupts the stream.
    #[serde(default)]
    pub body_hash: Option<BodyHash128>,
    /// Whether this is an unsafe function (Rust, C, etc.).
    ///
    /// For FFI functions, this indicates whether the function is declared with
    /// `unsafe` modifier (Haskell), `unsafe` keyword (Rust), or similar safety
    /// markers in other languages.
    ///
    /// CRITICAL: This field is placed AFTER `body_hash` to maintain postcard
    /// serialization compatibility. Adding fields before `body_hash` would
    /// corrupt existing snapshots due to positional deserialization.
    /// Uses `#[serde(default)]` to read older snapshots that lack this field.
    #[serde(default)]
    pub is_unsafe: bool,
}

impl NodeEntry {
    /// Creates a new node entry with minimal required fields.
    #[must_use]
    pub fn new(kind: NodeKind, name: StringId, file: FileId) -> Self {
        Self {
            kind,
            name,
            file,
            start_byte: 0,
            end_byte: 0,
            start_line: 0,
            start_column: 0,
            end_line: 0,
            end_column: 0,
            signature: None,
            doc: None,
            qualified_name: None,
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        }
    }

    /// Sets the byte range for this node.
    #[must_use]
    pub fn with_byte_range(mut self, start: u32, end: u32) -> Self {
        self.start_byte = start;
        self.end_byte = end;
        self
    }

    /// Sets the line/column range for this node.
    #[must_use]
    pub fn with_location(
        mut self,
        start_line: u32,
        start_column: u32,
        end_line: u32,
        end_column: u32,
    ) -> Self {
        self.start_line = start_line;
        self.start_column = start_column;
        self.end_line = end_line;
        self.end_column = end_column;
        self
    }

    /// Sets the signature for this node.
    #[must_use]
    pub fn with_signature(mut self, signature: StringId) -> Self {
        self.signature = Some(signature);
        self
    }

    /// Sets the docstring for this node.
    #[must_use]
    pub fn with_doc(mut self, doc: StringId) -> Self {
        self.doc = Some(doc);
        self
    }

    /// Sets the qualified name for this node.
    #[must_use]
    pub fn with_qualified_name(mut self, qualified_name: StringId) -> Self {
        self.qualified_name = Some(qualified_name);
        self
    }

    /// Sets the visibility modifier for this node.
    #[must_use]
    pub fn with_visibility(mut self, visibility: StringId) -> Self {
        self.visibility = Some(visibility);
        self
    }

    /// Sets the async flag for this node.
    #[must_use]
    pub fn with_async(mut self, is_async: bool) -> Self {
        self.is_async = is_async;
        self
    }

    /// Sets the static flag for this node.
    #[must_use]
    pub fn with_static(mut self, is_static: bool) -> Self {
        self.is_static = is_static;
        self
    }

    /// Sets the unsafe flag for this node.
    #[must_use]
    pub fn with_unsafe(mut self, is_unsafe: bool) -> Self {
        self.is_unsafe = is_unsafe;
        self
    }

    /// Sets the body hash for this node.
    ///
    /// The body hash is a 128-bit hash computed from the raw body bytes
    /// of the symbol. Used for duplicate code detection.
    #[must_use]
    pub fn with_body_hash(mut self, hash: BodyHash128) -> Self {
        self.body_hash = Some(hash);
        self
    }
}

/// Contiguous node storage with generational indices.
///
/// `NodeArena` provides O(1) access to nodes by `NodeId`, with stale
/// reference detection via generation counters.
///
/// # Free List Management
///
/// When a node is removed, its slot is added to the free list for reuse.
/// This avoids memory fragmentation and keeps indices stable.
///
/// # Example
///
/// ```rust,ignore
/// let mut arena = NodeArena::new();
/// let id = arena.alloc(NodeEntry::new(NodeKind::Function, name, file))?;
///
/// let node = arena.get(id).expect("node exists");
/// assert_eq!(node.kind, NodeKind::Function);
///
/// arena.remove(id)?;
/// assert!(arena.get(id).is_none()); // Stale ID returns None
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeArena {
    /// Slots for node storage.
    slots: Vec<Slot<NodeEntry>>,
    /// Head of the free list (index of first free slot).
    free_head: Option<u32>,
    /// Number of currently occupied slots.
    len: usize,
}

impl NodeArena {
    /// Creates a new empty arena.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_head: None,
            len: 0,
        }
    }

    /// Creates a new arena with the specified initial capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity),
            free_head: None,
            len: 0,
        }
    }

    /// Returns the number of occupied slots.
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns true if the arena is empty.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the allocated capacity (slots that can be stored without reallocation).
    #[inline]
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.capacity()
    }

    /// Returns the total number of slots (occupied + vacant).
    ///
    /// This is different from `len()` which counts only occupied slots,
    /// and `capacity()` which counts pre-allocated slots.
    #[inline]
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Allocates a new node and returns its `NodeId`.
    ///
    /// If there are free slots available, one is reused. Otherwise, a new
    /// slot is appended to the storage.
    ///
    /// # Errors
    ///
    /// Returns [`ArenaError`] if:
    /// - Generation counter would overflow (`GenerationOverflow`)
    /// - Free list is corrupted (`FreeListCorruption`)
    /// - Arena capacity exceeded (`CapacityExceeded`)
    pub fn alloc(&mut self, entry: NodeEntry) -> Result<NodeId, ArenaError> {
        self.len += 1;

        if let Some(free_idx) = self.free_head {
            // Check bounds before accessing slot - corrupted free list could point out of bounds
            let slot_index = free_idx as usize;
            if slot_index >= self.slots.len() {
                self.len -= 1; // Rollback
                return Err(ArenaError::FreeListCorruption { index: free_idx });
            }

            // Reuse a free slot (bounds now guaranteed valid)
            let slot = &mut self.slots[slot_index];

            // Update free list head
            self.free_head = match &slot.state {
                SlotState::Vacant { next_free } => *next_free,
                SlotState::Occupied(_) => {
                    // Free list corruption detected - return error instead of panicking
                    self.len -= 1; // Rollback
                    return Err(ArenaError::FreeListCorruption { index: free_idx });
                }
            };

            // Increment generation for reuse detection (respect MAX_GENERATION limit)
            // Use NodeId::try_increment_generation which checks against MAX_GENERATION, not u64::MAX
            let temp_id = NodeId::new(free_idx, slot.generation);
            let new_generation = temp_id.try_increment_generation().map_err(|mut e| {
                self.len -= 1; // Rollback
                e.index = free_idx; // Ensure index is correct
                ArenaError::GenerationOverflow(e)
            })?;

            slot.generation = new_generation;
            slot.state = SlotState::Occupied(entry);

            Ok(NodeId::new(free_idx, new_generation))
        } else {
            // Append a new slot
            let index = self.slots.len();

            // Check capacity before allocation using checked conversion
            let Ok(index_u32) = u32::try_from(index) else {
                self.len -= 1; // Rollback
                return Err(ArenaError::CapacityExceeded);
            };

            let slot = Slot::new_occupied(1, entry);
            self.slots.push(slot);

            Ok(NodeId::new(index_u32, 1))
        }
    }

    /// Returns a reference to the node with the given ID.
    ///
    /// Returns `None` if the ID is invalid or stale (generation mismatch).
    #[must_use]
    pub fn get(&self, id: NodeId) -> Option<&NodeEntry> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        let slot = self.slots.get(index)?;

        // Check generation match
        if slot.generation != id.generation() {
            return None;
        }

        slot.get()
    }

    /// Returns a mutable reference to the node with the given ID.
    ///
    /// Returns `None` if the ID is invalid or stale.
    #[must_use]
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut NodeEntry> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        let slot = self.slots.get_mut(index)?;

        // Check generation match
        if slot.generation != id.generation() {
            return None;
        }

        slot.get_mut()
    }

    /// Checks if the given ID is valid (not stale).
    #[must_use]
    pub fn contains(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    /// Removes the node with the given ID.
    ///
    /// Returns the removed entry, or `None` if the ID was invalid/stale.
    /// This method is idempotent: calling it twice with the same ID will
    /// return `None` on the second call without corrupting the arena.
    pub fn remove(&mut self, id: NodeId) -> Option<NodeEntry> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        let slot = self.slots.get_mut(index)?;

        // Check generation match
        if slot.generation != id.generation() {
            return None;
        }

        // Only proceed if slot is occupied - this makes remove idempotent
        // and prevents double-decrement of len / double-enqueue to free list
        if let SlotState::Occupied(_) = &slot.state {
            let old_state = std::mem::replace(
                &mut slot.state,
                SlotState::Vacant {
                    next_free: self.free_head,
                },
            );

            // Update free list and len only when actually removing
            self.free_head = Some(id.index());
            self.len -= 1;

            if let SlotState::Occupied(entry) = old_state {
                return Some(entry);
            }
        }

        None
    }

    /// Iterates over all occupied entries with their IDs.
    pub fn iter(&self) -> impl Iterator<Item = (NodeId, &NodeEntry)> {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            if let SlotState::Occupied(entry) = &slot.state {
                let index_u32 = u32::try_from(index).ok()?;
                Some((NodeId::new(index_u32, slot.generation), entry))
            } else {
                None
            }
        })
    }

    /// Iterates over all occupied entries with mutable access.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (NodeId, &mut NodeEntry)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(index, slot)| {
                let generation = slot.generation;
                if let SlotState::Occupied(entry) = &mut slot.state {
                    let index_u32 = u32::try_from(index).ok()?;
                    Some((NodeId::new(index_u32, generation), entry))
                } else {
                    None
                }
            })
    }

    /// Clears all nodes from the arena.
    ///
    /// This resets the arena to an empty state, but retains allocated capacity.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free_head = None;
        self.len = 0;
    }

    /// Reserves capacity for at least `additional` more nodes.
    pub fn reserve(&mut self, additional: usize) {
        self.slots.reserve(additional);
    }

    /// Returns the slot at the given index, if any.
    ///
    /// This is a low-level method for internal use. Prefer `get()` for normal access.
    #[must_use]
    pub fn slot(&self, index: u32) -> Option<&Slot<NodeEntry>> {
        self.slots.get(index as usize)
    }

    /// Pre-allocates a contiguous range of occupied slots for parallel commit.
    ///
    /// Returns the start index of the allocated range. All slots are initialized
    /// as `Occupied` with clones of the `placeholder` entry at generation 1.
    /// The free list is **not** touched, preserving its invariants.
    ///
    /// A `count` of zero is a no-op and returns the current slot count as the
    /// start index.
    ///
    /// # Errors
    ///
    /// Returns [`ArenaError::CapacityExceeded`] if the resulting slot count
    /// would exceed `u32::MAX`.
    pub fn alloc_range(&mut self, count: u32, placeholder: &NodeEntry) -> Result<u32, ArenaError> {
        if count == 0 {
            // No-op: return current slot count as start index (safe because
            // slot_count <= u32::MAX is maintained as an invariant).
            return Ok(u32::try_from(self.slots.len())
                .unwrap_or_else(|_| unreachable!("slot_count invariant violated")));
        }

        let start = self.slots.len();
        let new_total = start
            .checked_add(count as usize)
            .ok_or(ArenaError::CapacityExceeded)?;

        // Verify new total fits in u32 (arena index space).
        if new_total > u32::MAX as usize + 1 {
            return Err(ArenaError::CapacityExceeded);
        }

        let start_u32 = u32::try_from(start).map_err(|_| ArenaError::CapacityExceeded)?;

        // Reserve and fill in one go.
        self.slots.reserve(count as usize);
        for _ in 0..count {
            self.slots.push(Slot::new_occupied(1, placeholder.clone()));
        }

        // Increment occupied count immediately — arena state is always consistent.
        self.len += count as usize;

        Ok(start_u32)
    }

    /// Returns a mutable sub-slice of the internal slot storage.
    ///
    /// This is intended for parallel writes after [`alloc_range`](Self::alloc_range):
    /// each thread can write to its own non-overlapping portion of the slice.
    ///
    /// # Panics
    ///
    /// Panics if `start + count` exceeds the slot count.
    #[must_use]
    pub fn bulk_slice_mut(&mut self, start: u32, count: u32) -> &mut [Slot<NodeEntry>] {
        let begin = start as usize;
        let end = begin + count as usize;
        &mut self.slots[begin..end]
    }

    /// Truncates the slot storage to `saved_slot_count` slots.
    ///
    /// Used to roll back a failed bulk allocation. Any slots beyond
    /// `saved_slot_count` are dropped, and `len` is adjusted to reflect
    /// only the occupied slots that remain.
    ///
    /// # Panics
    ///
    /// Panics if `saved_slot_count` is greater than the current slot count
    /// (cannot grow via truncation).
    pub fn truncate_to(&mut self, saved_slot_count: usize) {
        assert!(
            saved_slot_count <= self.slots.len(),
            "truncate_to({saved_slot_count}) exceeds current slot count ({})",
            self.slots.len(),
        );

        // Count occupied slots that will be dropped.
        let dropped_occupied = self.slots[saved_slot_count..]
            .iter()
            .filter(|s| s.is_occupied())
            .count();

        self.slots.truncate(saved_slot_count);
        self.len -= dropped_occupied;

        // Rebuild free list: remove any entries that pointed into the truncated
        // region. We walk the chain and keep only links to retained slots.
        self.rebuild_free_list_after_truncate(saved_slot_count);
    }

    /// Rebuilds the free list after a truncation, dropping any links to slots
    /// at or beyond `cutoff`.
    ///
    /// Scans retained slots for vacant entries and re-links them into a fresh
    /// free list. This is O(cutoff) but `truncate_to` is a rare rollback path.
    fn rebuild_free_list_after_truncate(&mut self, cutoff: usize) {
        // Simple approach: scan retained slots for vacant entries.
        // This avoids issues with interleaved chains (e.g., head -> 6 -> 0)
        // where truncated indices appear mid-chain.
        let mut new_head: Option<u32> = None;
        // Walk backwards so the resulting list is in ascending index order
        // (head = lowest free index), matching the pattern of fresh arenas.
        for i in (0..cutoff).rev() {
            if self.slots[i].is_vacant() {
                let i_u32 = u32::try_from(i)
                    .unwrap_or_else(|_| unreachable!("free-list index exceeds u32 invariant"));
                self.slots[i].state = SlotState::Vacant {
                    next_free: new_head,
                };
                new_head = Some(i_u32);
            }
        }
        self.free_head = new_head;
    }

    /// Returns a read-only view of all slots (occupied and vacant).
    ///
    /// Useful for post-build passes that need to scan every slot without
    /// the overhead of generation checks.
    #[must_use]
    pub fn slots(&self) -> &[Slot<NodeEntry>] {
        &self.slots
    }
}

impl Default for NodeArena {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for NodeArena {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "NodeArena(len={}, capacity={})",
            self.len,
            self.capacity()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_file() -> FileId {
        FileId::new(1)
    }

    fn test_name() -> StringId {
        StringId::new(1)
    }

    fn test_entry(kind: NodeKind) -> NodeEntry {
        NodeEntry::new(kind, test_name(), test_file())
    }

    #[test]
    fn test_arena_new() {
        let arena = NodeArena::new();
        assert_eq!(arena.len(), 0);
        assert!(arena.is_empty());
        assert_eq!(arena.capacity(), 0);
    }

    #[test]
    fn test_arena_with_capacity() {
        let arena = NodeArena::with_capacity(100);
        assert_eq!(arena.len(), 0);
        assert!(arena.capacity() >= 100);
    }

    #[test]
    fn test_alloc_and_get() {
        let mut arena = NodeArena::new();
        let entry = test_entry(NodeKind::Function);

        let id = arena.alloc(entry).unwrap();
        assert!(!id.is_invalid());
        assert_eq!(arena.len(), 1);

        let node = arena.get(id).unwrap();
        assert_eq!(node.kind, NodeKind::Function);
    }

    #[test]
    fn test_alloc_multiple() {
        let mut arena = NodeArena::new();

        let id1 = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let id2 = arena.alloc(test_entry(NodeKind::Class)).unwrap();
        let id3 = arena.alloc(test_entry(NodeKind::Method)).unwrap();

        assert_eq!(arena.len(), 3);
        assert_ne!(id1, id2);
        assert_ne!(id2, id3);

        assert_eq!(arena.get(id1).unwrap().kind, NodeKind::Function);
        assert_eq!(arena.get(id2).unwrap().kind, NodeKind::Class);
        assert_eq!(arena.get(id3).unwrap().kind, NodeKind::Method);
    }

    #[test]
    fn test_get_mut() {
        let mut arena = NodeArena::new();
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();

        {
            let node = arena.get_mut(id).unwrap();
            node.start_line = 42;
        }

        assert_eq!(arena.get(id).unwrap().start_line, 42);
    }

    #[test]
    fn test_contains() {
        let mut arena = NodeArena::new();
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();

        assert!(arena.contains(id));
        assert!(!arena.contains(NodeId::INVALID));
    }

    #[test]
    fn test_remove() {
        let mut arena = NodeArena::new();
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();

        assert_eq!(arena.len(), 1);
        assert!(arena.contains(id));

        let removed = arena.remove(id).unwrap();
        assert_eq!(removed.kind, NodeKind::Function);
        assert_eq!(arena.len(), 0);
        assert!(!arena.contains(id));
    }

    #[test]
    fn test_stale_id_after_remove() {
        let mut arena = NodeArena::new();
        let id1 = arena.alloc(test_entry(NodeKind::Function)).unwrap();

        // Remove and reallocate
        arena.remove(id1);
        let id2 = arena.alloc(test_entry(NodeKind::Class)).unwrap();

        // Old ID should be stale
        assert!(arena.get(id1).is_none());
        // New ID should work
        assert_eq!(arena.get(id2).unwrap().kind, NodeKind::Class);
        // They should have the same index but different generations
        assert_eq!(id1.index(), id2.index());
        assert_ne!(id1.generation(), id2.generation());
    }

    #[test]
    fn test_remove_idempotent() {
        let mut arena = NodeArena::new();
        let id1 = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let id2 = arena.alloc(test_entry(NodeKind::Class)).unwrap();

        // First remove should succeed
        assert!(arena.remove(id1).is_some());
        assert_eq!(arena.len(), 1);

        // Second remove of same ID should be idempotent (no side effects)
        assert!(arena.remove(id1).is_none());
        assert_eq!(arena.len(), 1); // len unchanged

        // Third remove should still be safe
        assert!(arena.remove(id1).is_none());
        assert_eq!(arena.len(), 1); // len still unchanged

        // Other entries should be unaffected
        assert!(arena.contains(id2));

        // New allocation should work correctly
        let id3 = arena.alloc(test_entry(NodeKind::Method)).unwrap();
        assert_eq!(id3.index(), id1.index()); // Reuses the removed slot
        assert_eq!(arena.len(), 2);
    }

    #[test]
    fn test_free_list_reuse() {
        let mut arena = NodeArena::new();

        // Allocate 3 nodes
        let id1 = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let id2 = arena.alloc(test_entry(NodeKind::Class)).unwrap();
        let id3 = arena.alloc(test_entry(NodeKind::Method)).unwrap();

        // Remove middle node
        arena.remove(id2);
        assert_eq!(arena.len(), 2);
        assert_eq!(arena.slot_count(), 3);

        // Allocate new node - should reuse id2's slot
        let id4 = arena.alloc(test_entry(NodeKind::Variable)).unwrap();
        assert_eq!(id4.index(), id2.index());
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.slot_count(), 3); // No growth - slot reused

        // Old id2 is stale
        assert!(arena.get(id2).is_none());
        // New id4 works
        assert_eq!(arena.get(id4).unwrap().kind, NodeKind::Variable);
        // Other IDs still valid
        assert!(arena.contains(id1));
        assert!(arena.contains(id3));
    }

    #[test]
    fn test_invalid_id() {
        let arena = NodeArena::new();
        assert!(arena.get(NodeId::INVALID).is_none());
    }

    #[test]
    fn test_out_of_bounds_id() {
        let arena = NodeArena::new();
        let fake_id = NodeId::new(999, 1);
        assert!(arena.get(fake_id).is_none());
    }

    #[test]
    fn test_wrong_generation() {
        let mut arena = NodeArena::new();
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();

        // Create ID with wrong generation
        let wrong_gen_id = NodeId::new(id.index(), id.generation() + 1);
        assert!(arena.get(wrong_gen_id).is_none());
    }

    #[test]
    fn test_iter() {
        let mut arena = NodeArena::new();
        arena.alloc(test_entry(NodeKind::Function)).unwrap();
        arena.alloc(test_entry(NodeKind::Class)).unwrap();
        arena.alloc(test_entry(NodeKind::Method)).unwrap();

        let entries: Vec<_> = arena.iter().collect();
        assert_eq!(entries.len(), 3);

        let kinds: Vec<_> = entries.iter().map(|(_, e)| e.kind).collect();
        assert!(kinds.contains(&NodeKind::Function));
        assert!(kinds.contains(&NodeKind::Class));
        assert!(kinds.contains(&NodeKind::Method));
    }

    #[test]
    fn test_iter_with_removed() {
        let mut arena = NodeArena::new();
        let id1 = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let id2 = arena.alloc(test_entry(NodeKind::Class)).unwrap();
        let id3 = arena.alloc(test_entry(NodeKind::Method)).unwrap();

        arena.remove(id2);

        let entries: Vec<_> = arena.iter().collect();
        assert_eq!(entries.len(), 2);

        let collected_ids: Vec<_> = entries.iter().map(|(id, _)| *id).collect();
        assert!(collected_ids.contains(&id1));
        assert!(!collected_ids.contains(&id2));
        assert!(collected_ids.contains(&id3));
    }

    #[test]
    fn test_iter_mut() {
        let mut arena = NodeArena::new();
        arena.alloc(test_entry(NodeKind::Function)).unwrap();
        arena.alloc(test_entry(NodeKind::Class)).unwrap();

        for (_, entry) in arena.iter_mut() {
            entry.start_line = 100;
        }

        for (_, entry) in arena.iter() {
            assert_eq!(entry.start_line, 100);
        }
    }

    #[test]
    fn test_clear() {
        let mut arena = NodeArena::new();
        arena.alloc(test_entry(NodeKind::Function)).unwrap();
        arena.alloc(test_entry(NodeKind::Class)).unwrap();

        assert_eq!(arena.len(), 2);
        arena.clear();
        assert_eq!(arena.len(), 0);
        assert!(arena.is_empty());
    }

    #[test]
    fn test_reserve() {
        let mut arena = NodeArena::new();
        arena.reserve(1000);
        assert!(arena.capacity() >= 1000);
    }

    #[test]
    fn test_display() {
        let mut arena = NodeArena::new();
        arena.alloc(test_entry(NodeKind::Function)).unwrap();

        let display = format!("{arena}");
        assert!(display.contains("NodeArena"));
        assert!(display.contains("len=1"));
    }

    #[test]
    fn test_node_entry_builder() {
        let entry = NodeEntry::new(NodeKind::Function, test_name(), test_file())
            .with_byte_range(10, 100)
            .with_location(1, 0, 5, 10)
            .with_signature(StringId::new(2))
            .with_doc(StringId::new(3))
            .with_qualified_name(StringId::new(4));

        assert_eq!(entry.kind, NodeKind::Function);
        assert_eq!(entry.start_byte, 10);
        assert_eq!(entry.end_byte, 100);
        assert_eq!(entry.start_line, 1);
        assert_eq!(entry.start_column, 0);
        assert_eq!(entry.end_line, 5);
        assert_eq!(entry.end_column, 10);
        assert_eq!(entry.signature, Some(StringId::new(2)));
        assert_eq!(entry.doc, Some(StringId::new(3)));
        assert_eq!(entry.qualified_name, Some(StringId::new(4)));
    }

    #[test]
    fn test_slot_state() {
        let occupied: Slot<i32> = Slot::new_occupied(1, 42);
        assert!(occupied.is_occupied());
        assert!(!occupied.is_vacant());
        assert_eq!(occupied.get(), Some(&42));

        let vacant: Slot<i32> = Slot::new_vacant(2, Some(5));
        assert!(!vacant.is_occupied());
        assert!(vacant.is_vacant());
        assert_eq!(vacant.get(), None);
    }

    #[test]
    fn test_slot_generation() {
        let slot: Slot<i32> = Slot::new_occupied(42, 100);
        assert_eq!(slot.generation(), 42);
    }

    #[test]
    fn test_slot_state_accessor() {
        let occupied: Slot<i32> = Slot::new_occupied(1, 42);
        assert!(matches!(occupied.state(), SlotState::Occupied(42)));

        let vacant: Slot<i32> = Slot::new_vacant(1, Some(3));
        assert!(matches!(
            vacant.state(),
            SlotState::Vacant { next_free: Some(3) }
        ));
    }

    // ---- Bulk API tests ----

    #[test]
    fn test_alloc_range_basic() {
        let mut arena = NodeArena::new();
        let placeholder = test_entry(NodeKind::Function);

        let start = arena.alloc_range(5, &placeholder).unwrap();
        assert_eq!(start, 0);
        assert_eq!(arena.len(), 5);
        assert_eq!(arena.slot_count(), 5);

        // All slots should be occupied with generation 1.
        for i in 0..5u32 {
            let id = NodeId::new(i, 1);
            let entry = arena.get(id).expect("slot should be occupied");
            assert_eq!(entry.kind, NodeKind::Function);
        }
    }

    #[test]
    fn test_alloc_range_after_existing() {
        let mut arena = NodeArena::new();

        // Allocate one node via normal path.
        let id0 = arena.alloc(test_entry(NodeKind::Class)).unwrap();
        assert_eq!(id0.index(), 0);
        assert_eq!(arena.len(), 1);

        // Bulk allocate 3 more.
        let placeholder = test_entry(NodeKind::Method);
        let start = arena.alloc_range(3, &placeholder).unwrap();
        assert_eq!(start, 1);
        assert_eq!(arena.len(), 4);
        assert_eq!(arena.slot_count(), 4);

        // Original node intact.
        assert_eq!(arena.get(id0).unwrap().kind, NodeKind::Class);

        // Bulk-allocated nodes accessible.
        for i in 1..4u32 {
            let id = NodeId::new(i, 1);
            assert_eq!(arena.get(id).unwrap().kind, NodeKind::Method);
        }
    }

    #[test]
    fn test_alloc_range_zero_is_noop() {
        let mut arena = NodeArena::new();

        // Allocate one normally first.
        arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let len_before = arena.len();
        let slot_count_before = arena.slot_count();

        let start = arena.alloc_range(0, &test_entry(NodeKind::Class)).unwrap();
        assert_eq!(start, slot_count_before as u32);
        assert_eq!(arena.len(), len_before);
        assert_eq!(arena.slot_count(), slot_count_before);
    }

    #[test]
    fn test_bulk_slice_mut() {
        let mut arena = NodeArena::new();
        let placeholder = test_entry(NodeKind::Function);
        let start = arena.alloc_range(3, &placeholder).unwrap();

        // Overwrite slot 1 via bulk_slice_mut.
        {
            let slice = arena.bulk_slice_mut(start, 3);
            assert_eq!(slice.len(), 3);

            // Replace the middle slot with a Class entry.
            slice[1] = Slot::new_occupied(1, test_entry(NodeKind::Class));
        }

        // Verify the overwrite via get().
        let id1 = NodeId::new(1, 1);
        assert_eq!(arena.get(id1).unwrap().kind, NodeKind::Class);

        // Other slots unchanged.
        let id0 = NodeId::new(0, 1);
        assert_eq!(arena.get(id0).unwrap().kind, NodeKind::Function);
        let id2 = NodeId::new(2, 1);
        assert_eq!(arena.get(id2).unwrap().kind, NodeKind::Function);
    }

    #[test]
    fn test_truncate_to() {
        let mut arena = NodeArena::new();
        let placeholder = test_entry(NodeKind::Function);

        // Allocate initial range.
        arena.alloc_range(3, &placeholder).unwrap();
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.slot_count(), 3);

        // Save state.
        let saved = arena.slot_count();

        // Allocate more.
        arena.alloc_range(4, &placeholder).unwrap();
        assert_eq!(arena.len(), 7);
        assert_eq!(arena.slot_count(), 7);

        // Truncate back.
        arena.truncate_to(saved);
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.slot_count(), 3);

        // Original slots still accessible.
        for i in 0..3u32 {
            let id = NodeId::new(i, 1);
            assert!(arena.get(id).is_some());
        }
    }

    #[test]
    fn test_alloc_range_with_free_list() {
        let mut arena = NodeArena::new();

        // Allocate two nodes, then remove the first to create a free list entry.
        let id0 = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let _id1 = arena.alloc(test_entry(NodeKind::Class)).unwrap();
        arena.remove(id0);

        // State: len=1, slot_count=2, free_head=Some(0)
        assert_eq!(arena.len(), 1);
        assert_eq!(arena.slot_count(), 2);

        // alloc_range should append AFTER slot_count, not reuse free list.
        let start = arena.alloc_range(3, &test_entry(NodeKind::Method)).unwrap();
        assert_eq!(start, 2); // Appended after existing 2 slots
        assert_eq!(arena.len(), 4); // 1 existing + 3 new
        assert_eq!(arena.slot_count(), 5); // 2 existing + 3 new

        // Free list should still be intact — slot 0 still vacant.
        let slot0 = arena.slot(0).unwrap();
        assert!(slot0.is_vacant());

        // Bulk-allocated nodes accessible.
        for i in 2..5u32 {
            let id = NodeId::new(i, 1);
            assert_eq!(arena.get(id).unwrap().kind, NodeKind::Method);
        }
    }

    #[test]
    fn test_truncate_to_clamps_free_head() {
        let mut arena = NodeArena::new();

        // Allocate 5 nodes.
        let mut ids = Vec::new();
        for _ in 0..5 {
            ids.push(arena.alloc(test_entry(NodeKind::Function)).unwrap());
        }

        // Save state at 5 slots.
        let saved = arena.slot_count();

        // Allocate 3 more, then remove one of them to put it on the free list.
        let extra_ids: Vec<_> = (0..3)
            .map(|_| arena.alloc(test_entry(NodeKind::Class)).unwrap())
            .collect();
        arena.remove(extra_ids[1]); // free_head now points to index 6

        // Truncate back — free_head should be clamped since index 6 is gone.
        arena.truncate_to(saved);
        assert_eq!(arena.slot_count(), 5);
        assert_eq!(arena.len(), 5);

        // Arena should still be usable: new alloc appends (no dangling free list).
        let new_id = arena.alloc(test_entry(NodeKind::Variable)).unwrap();
        assert_eq!(new_id.index(), 5); // Appended, not reusing dangling slot
        assert_eq!(arena.get(new_id).unwrap().kind, NodeKind::Variable);
    }

    #[test]
    fn test_truncate_to_preserves_retained_free_list() {
        let mut arena = NodeArena::new();

        // Allocate 8 nodes: indices 0..7
        let mut ids = Vec::new();
        for _ in 0..8 {
            ids.push(arena.alloc(test_entry(NodeKind::Function)).unwrap());
        }
        assert_eq!(arena.len(), 8);

        // Remove indices 2 and 6 — both on free list.
        // Free list: 6 -> 2 -> None (LIFO)
        arena.remove(ids[2]);
        arena.remove(ids[6]);
        assert_eq!(arena.len(), 6);

        // Truncate to 5 slots — removes indices 5,6,7.
        // Index 6 (vacant) is in truncated region, index 2 (vacant) is retained.
        // Free list chain was 6 -> 2. After truncate, only index 2 should survive.
        arena.truncate_to(5);

        // Should have 4 occupied (indices 0,1,3,4) + 1 vacant (index 2) = 5 slots.
        // len should be 4 (we had 6 occupied, truncated 3 slots: 5 occupied + 1 vacant
        // in truncated region = indices 5(occ), 6(vac), 7(occ) => dropped 2 occupied).
        assert_eq!(arena.slot_count(), 5);
        assert_eq!(arena.len(), 4);

        // Free list should contain only index 2.
        // Verify by allocating — should reuse index 2.
        let reused = arena.alloc(test_entry(NodeKind::Variable)).unwrap();
        assert_eq!(reused.index(), 2);
        assert_eq!(arena.get(reused).unwrap().kind, NodeKind::Variable);
        assert_eq!(arena.len(), 5);

        // Next alloc should append (no more free slots).
        let appended = arena.alloc(test_entry(NodeKind::Class)).unwrap();
        assert_eq!(appended.index(), 5);
        assert_eq!(arena.len(), 6);
    }

    #[test]
    fn test_slots_read_access() {
        let mut arena = NodeArena::new();
        let placeholder = test_entry(NodeKind::Variable);
        arena.alloc_range(4, &placeholder).unwrap();

        let slots = arena.slots();
        assert_eq!(slots.len(), 4);

        for slot in slots {
            assert!(slot.is_occupied());
            assert_eq!(slot.generation(), 1);
            assert_eq!(slot.get().unwrap().kind, NodeKind::Variable);
        }
    }

    #[test]
    fn test_multiple_remove_reuse_cycle() {
        let mut arena = NodeArena::new();

        // Allocate and remove multiple times to test free list chaining
        let mut ids = Vec::new();
        for _ in 0..5 {
            ids.push(arena.alloc(test_entry(NodeKind::Function)).unwrap());
        }

        // Remove in reverse order (builds free list: 4 -> 3 -> 2 -> 1 -> 0)
        for &id in ids.iter().rev() {
            arena.remove(id);
        }

        // Reallocate - should use free list in LIFO order
        let new_ids: Vec<_> = (0..5)
            .map(|_| arena.alloc(test_entry(NodeKind::Class)).unwrap())
            .collect();

        // All old IDs should be stale
        for &old_id in &ids {
            assert!(arena.get(old_id).is_none());
        }

        // All new IDs should work
        for &new_id in &new_ids {
            assert!(arena.get(new_id).is_some());
        }
    }

    #[test]
    fn test_generation_overflow_at_max_generation() {
        // Test that allocation fails when generation reaches MAX_GENERATION
        let mut arena = NodeArena::new();

        // Allocate a node
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        assert_eq!(id.index(), 0);

        // Remove it to put on free list
        arena.remove(id);

        // Manually set the slot's generation to MAX_GENERATION
        // This simulates what would happen after ~9.2 quintillion allocations
        arena.slots[0].generation = NodeId::MAX_GENERATION;

        // Now allocation should fail because incrementing MAX_GENERATION would overflow
        let result = arena.alloc(test_entry(NodeKind::Class));
        assert!(result.is_err());

        let err = result.unwrap_err();
        match err {
            ArenaError::GenerationOverflow(inner) => {
                assert_eq!(inner.index, 0);
                assert_eq!(inner.generation, NodeId::MAX_GENERATION);
            }
            _ => panic!("expected GenerationOverflow, got {err:?}"),
        }
    }

    #[test]
    fn test_free_list_corruption_returns_error() {
        // Test that free list corruption returns an error instead of panicking
        let mut arena = NodeArena::new();

        // Allocate and remove a node to put it on free list
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        arena.remove(id);

        // Corrupt the free list by manually marking the vacant slot as occupied
        arena.slots[0].state = SlotState::Occupied(test_entry(NodeKind::Class));

        // Allocation should fail gracefully with FreeListCorruption error
        let result = arena.alloc(test_entry(NodeKind::Method));
        assert!(result.is_err());

        let err = result.unwrap_err();
        match err {
            ArenaError::FreeListCorruption { index } => {
                assert_eq!(index, 0);
            }
            _ => panic!("expected FreeListCorruption, got {err:?}"),
        }

        // Verify the arena is still in a consistent state (len was rolled back)
        assert_eq!(arena.len(), 0);
    }

    #[test]
    fn test_arena_error_display() {
        let gen_err = ArenaError::GenerationOverflow(GenerationOverflowError {
            index: 42,
            generation: 100,
        });
        let display = format!("{gen_err}");
        assert!(display.contains("42"));
        assert!(display.contains("100"));

        let corruption_err = ArenaError::FreeListCorruption { index: 5 };
        let display = format!("{corruption_err}");
        assert!(display.contains("free list corruption"));
        assert!(display.contains("5"));

        let capacity_err = ArenaError::CapacityExceeded;
        let display = format!("{capacity_err}");
        assert!(display.contains("capacity exceeded"));
    }

    #[test]
    fn test_arena_error_source_generation_overflow() {
        use std::error::Error;

        let inner = GenerationOverflowError {
            index: 0,
            generation: NodeId::MAX_GENERATION,
        };
        let err = ArenaError::GenerationOverflow(inner);
        // source() returns Some for GenerationOverflow
        assert!(err.source().is_some());
    }

    #[test]
    fn test_arena_error_source_none_for_other_variants() {
        use std::error::Error;

        let corruption = ArenaError::FreeListCorruption { index: 0 };
        assert!(corruption.source().is_none());

        let capacity = ArenaError::CapacityExceeded;
        assert!(capacity.source().is_none());
    }

    #[test]
    fn test_arena_from_generation_overflow_error() {
        let inner = GenerationOverflowError {
            index: 10,
            generation: 99,
        };
        let err: ArenaError = inner.into();
        assert!(matches!(err, ArenaError::GenerationOverflow(_)));
    }

    #[test]
    fn test_arena_error_clone_and_eq() {
        let err = ArenaError::CapacityExceeded;
        let cloned = err.clone();
        assert_eq!(err, cloned);

        let err2 = ArenaError::FreeListCorruption { index: 3 };
        let cloned2 = err2.clone();
        assert_eq!(err2, cloned2);
    }

    #[test]
    fn test_node_entry_with_visibility() {
        let entry = NodeEntry::new(NodeKind::Function, test_name(), test_file())
            .with_visibility(StringId::new(5));

        assert_eq!(entry.visibility, Some(StringId::new(5)));
    }

    #[test]
    fn test_node_entry_with_async_and_static() {
        let entry = NodeEntry::new(NodeKind::Method, test_name(), test_file())
            .with_async(true)
            .with_static(true);

        assert!(entry.is_async);
        assert!(entry.is_static);
    }

    #[test]
    fn test_node_entry_with_unsafe_flag() {
        let entry = NodeEntry::new(NodeKind::Function, test_name(), test_file()).with_unsafe(true);

        assert!(entry.is_unsafe);
    }

    #[test]
    fn test_node_entry_with_body_hash() {
        use crate::graph::body_hash::BodyHash128;
        let hash = BodyHash128::from_u128(0u128);
        let entry =
            NodeEntry::new(NodeKind::Function, test_name(), test_file()).with_body_hash(hash);

        assert!(entry.body_hash.is_some());
    }

    #[test]
    fn test_node_entry_defaults() {
        let entry = NodeEntry::new(NodeKind::Function, test_name(), test_file());
        assert_eq!(entry.start_byte, 0);
        assert_eq!(entry.end_byte, 0);
        assert_eq!(entry.start_line, 0);
        assert_eq!(entry.start_column, 0);
        assert_eq!(entry.end_line, 0);
        assert_eq!(entry.end_column, 0);
        assert!(entry.signature.is_none());
        assert!(entry.doc.is_none());
        assert!(entry.qualified_name.is_none());
        assert!(entry.visibility.is_none());
        assert!(!entry.is_async);
        assert!(!entry.is_static);
        assert!(!entry.is_unsafe);
        assert!(entry.body_hash.is_none());
    }

    #[test]
    fn test_arena_default_impl() {
        let arena = NodeArena::default();
        assert_eq!(arena.len(), 0);
        assert!(arena.is_empty());
    }

    #[test]
    fn test_get_invalid_id() {
        let mut arena = NodeArena::new();
        arena.alloc(test_entry(NodeKind::Function)).unwrap();
        // get with INVALID should return None
        assert!(arena.get(NodeId::INVALID).is_none());
    }

    #[test]
    fn test_get_mut_invalid_id() {
        let mut arena = NodeArena::new();
        arena.alloc(test_entry(NodeKind::Function)).unwrap();
        assert!(arena.get_mut(NodeId::INVALID).is_none());
    }

    #[test]
    fn test_get_mut_wrong_generation() {
        let mut arena = NodeArena::new();
        let id = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        let wrong_gen_id = NodeId::new(id.index(), id.generation() + 1);
        assert!(arena.get_mut(wrong_gen_id).is_none());
    }

    #[test]
    fn test_remove_invalid_id() {
        let mut arena = NodeArena::new();
        assert!(arena.remove(NodeId::INVALID).is_none());
    }

    #[test]
    fn test_slot_get_mut_occupied() {
        let mut slot: Slot<i32> = Slot::new_occupied(1, 42);
        let val = slot.get_mut().unwrap();
        *val = 99;
        assert_eq!(slot.get(), Some(&99));
    }

    #[test]
    fn test_slot_get_mut_vacant() {
        let mut slot: Slot<i32> = Slot::new_vacant(1, None);
        assert!(slot.get_mut().is_none());
    }

    #[test]
    fn test_truncate_to_zero_occupied_dropped() {
        let mut arena = NodeArena::new();
        let placeholder = test_entry(NodeKind::Function);
        arena.alloc_range(5, &placeholder).unwrap();
        // Truncate to 0 — all 5 occupied slots are dropped
        arena.truncate_to(0);
        assert_eq!(arena.len(), 0);
        assert_eq!(arena.slot_count(), 0);
    }

    #[test]
    fn test_slot_count_vs_len() {
        let mut arena = NodeArena::new();
        let id0 = arena.alloc(test_entry(NodeKind::Function)).unwrap();
        arena.alloc(test_entry(NodeKind::Class)).unwrap();
        arena.remove(id0);
        // slot_count is total slots (including vacant), len is occupied only
        assert_eq!(arena.slot_count(), 2);
        assert_eq!(arena.len(), 1);
    }
}
