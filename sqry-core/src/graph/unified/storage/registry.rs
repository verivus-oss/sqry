//! `FileRegistry`: Path deduplication with `FileId` handles.
//!
//! This module implements `FileRegistry`, which provides efficient storage
//! of file paths by deduplicating identical canonical paths.
//!
//! # Design
//!
//! - **Path deduplication**: Same canonical path returns same `FileId`
//! - **Canonical normalization**: Paths are canonicalized before registration
//! - **Bi-directional lookup**: ID → path and path → ID
//!
//! # Thread Safety
//!
//! The registry uses `Arc<Path>` for path storage. However, the registry itself
//! requires external synchronization (e.g., `RwLock`) for concurrent access.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::super::file::id::FileId;
use super::super::node::id::NodeId;
use super::super::string::id::StringId;
use crate::graph::node::Language;

/// Error returned when a file path cannot be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Path canonicalization failed.
    CanonicalizationFailed {
        /// The original path
        path: PathBuf,
        /// Error message
        message: String,
    },
    /// Registry capacity exhausted.
    CapacityExhausted,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CanonicalizationFailed { path, message } => {
                write!(
                    f,
                    "failed to canonicalize path '{}': {}",
                    path.display(),
                    message
                )
            }
            Self::CapacityExhausted => {
                write!(f, "file registry capacity exhausted (> 2^32 files)")
            }
        }
    }
}

impl std::error::Error for RegistryError {}

/// File entry storing path, language, and Phase 1 provenance.
///
/// This struct is **module-private**. External code reads provenance via
/// [`FileRegistry::file_provenance`], which returns a borrowed
/// [`FileProvenanceView`]. Phase 1 growth of this struct is an
/// implementation detail and does not affect the public API.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileEntry {
    /// The canonical file path.
    path: Arc<Path>,
    /// The language of this file (if known).
    language: Option<Language>,
    /// Whether this file originates from an external source (e.g., classpath JAR).
    #[serde(default)]
    is_external: bool,
    /// SHA-256 of the on-disk file bytes at the most recent indexing pass.
    ///
    /// Populated by the build pipeline (P1U08) via
    /// [`FileRegistry::set_content_hash`]. Defaulted to all-zero bytes for
    /// legacy V7 snapshots loaded through the backwards-read path, and for
    /// entries inserted before the hash is known.
    #[serde(default = "default_content_hash")]
    content_hash: [u8; 32],
    /// Unix-epoch seconds at which this file was registered in the most
    /// recent build. Equal to the snapshot's `fact_epoch` across every
    /// `FileEntry` in a single build.
    #[serde(default)]
    indexed_at: u64,
    /// Interned physical origin URI for external or synthetic files —
    /// e.g. `jar:file:///…/rt.jar!/java/lang/Object.class` for classpath
    /// entries. `Some` iff the physical path alone is insufficient to
    /// identify the origin.
    ///
    /// Invariant (eventual): once external registration is complete,
    /// `is_external == true` implies `source_uri.is_some()`. The
    /// two-phase path (`register_external` followed by `set_source_uri`)
    /// may temporarily leave `source_uri` unset until the URI is stamped.
    #[serde(default)]
    source_uri: Option<StringId>,
}

/// Default content hash for `FileEntry` fields deserialized from legacy
/// snapshots that predate Phase 1 or for entries inserted before the build
/// pipeline stamps a real hash.
#[inline]
fn default_content_hash() -> [u8; 32] {
    [0u8; 32]
}

/// Borrowed view of per-file provenance, returned by
/// [`FileRegistry::file_provenance`].
///
/// This is the stable public surface the Phase 1 accessor exposes. The
/// underlying [`FileEntry`] remains module-private; growing it is an
/// implementation detail of the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileProvenanceView<'a> {
    /// Borrowed SHA-256 of the on-disk file bytes.
    pub content_hash: &'a [u8; 32],
    /// Unix-epoch seconds at which this file was registered.
    pub indexed_at: u64,
    /// Optional interned physical origin URI (see [`FileEntry::source_uri`]).
    pub source_uri: Option<StringId>,
    /// Whether this file originates from an external source.
    pub is_external: bool,
}

/// File registry for path deduplication.
///
/// `FileRegistry` stores file paths efficiently by maintaining a single
/// copy of each unique canonical path. When the same path is registered
/// multiple times (even with different formats), the same `FileId` is returned.
///
/// # Path Normalization
///
/// Paths are normalized before registration using a best-effort canonicalization:
/// - Relative paths are converted to absolute
/// - Symlinks are resolved (when possible)
/// - Path separators are normalized
///
/// # Language Tracking
///
/// Each file can optionally have an associated `Language` for filtering and
/// visualization purposes. Language can be set during registration or updated later.
///
/// # Example
///
/// ```rust,ignore
/// let mut registry = FileRegistry::new();
///
/// let id1 = registry.register(Path::new("src/lib.rs"))?;
/// let id2 = registry.register(Path::new("./src/../src/lib.rs"))?;
/// assert_eq!(id1, id2); // Same canonical path → same ID
///
/// // Set language
/// registry.set_language(id1, Language::Rust);
///
/// let path = registry.resolve(id1).unwrap();
/// assert!(path.ends_with("src/lib.rs"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRegistry {
    /// Storage of registered file entries, indexed by `FileId`.
    entries: Vec<Option<FileEntry>>,
    /// Reverse lookup from canonical path to index.
    lookup: HashMap<Arc<Path>, u32>,
    /// Free list of recycled slot indices.
    free_list: Vec<u32>,
    /// Per-file node buckets — every `NodeId` committed into the arena for
    /// a given `FileId` is appended here during parallel-parse commit.
    ///
    /// This is the live source of truth for the Gate 0c `NodeIdBearing`
    /// impl (A2 §K row K.B1), the Gate 0c finalize step 6 compaction, and
    /// the Gate 0c / 0d bucket-bijection debug invariant on
    /// [`super::super::concurrent::CodeGraph::assert_bucket_bijection`].
    ///
    /// Serialization: the map **is** persisted so V7+ snapshots carry the
    /// bucket data directly; an empty map on load (legacy V7 snapshots
    /// that predate the field) is indistinguishable from a graph that
    /// happens to contain no nodes yet, so the bijection check's
    /// "non-empty → strict" behaviour handles both transparently.
    #[serde(default)]
    per_file_nodes: HashMap<FileId, Vec<NodeId>>,
}

impl FileRegistry {
    /// Creates a new empty file registry.
    #[must_use]
    pub fn new() -> Self {
        // Reserve index 0 for INVALID sentinel
        Self {
            entries: vec![None],
            lookup: HashMap::new(),
            free_list: Vec::new(),
            per_file_nodes: HashMap::new(),
        }
    }

    /// Creates a new registry with the specified capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut entries = Vec::with_capacity(capacity + 1);
        entries.push(None); // Reserve index 0

        Self {
            entries,
            lookup: HashMap::with_capacity(capacity),
            free_list: Vec::new(),
            per_file_nodes: HashMap::with_capacity(capacity),
        }
    }

    /// Returns the number of registered files (excluding INVALID slot).
    #[inline]
    #[must_use]
    pub fn len(&self) -> usize {
        self.lookup.len()
    }

    /// Returns true if no files are registered.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lookup.is_empty()
    }

    /// Returns an iterator over the per-file node buckets used by the
    /// Gate 0c/0d bucket-bijection check (A2 §F.1).
    ///
    /// Yields `(FileId, Vec<NodeId>)` tuples, one per file that has at
    /// least one recorded `NodeId`. Each returned `Vec<NodeId>` is a
    /// **clone** of the internal bucket; the registry continues to own
    /// the canonical storage, so callers must not assume identity
    /// between repeated invocations. The iterator's ordering is whatever
    /// `HashMap::iter` produces — stable per single call, but not across
    /// calls.
    ///
    /// The Gate 0c `RebuildGraph::finalize()` contract calls the
    /// bijection check unconditionally in debug builds. When no nodes
    /// have been recorded yet (e.g. an empty graph, or a legacy V7
    /// snapshot whose per-file buckets have not been rebuilt), this
    /// iterator is empty and the check's "non-empty → strict" guard
    /// makes it vacuous — exactly the behaviour every harness requires.
    pub fn per_file_nodes_for_gate0d(
        &self,
    ) -> impl Iterator<
        Item = (
            crate::graph::unified::file::FileId,
            Vec<crate::graph::unified::node::NodeId>,
        ),
    > + '_ {
        self.per_file_nodes
            .iter()
            .map(|(&fid, nodes)| (fid, nodes.clone()))
    }

    /// Append `node` to the bucket for `file`.
    ///
    /// Called by `phase3_parallel_commit` immediately after a node is
    /// written into the arena. Duplicates are not dedup'd here — the
    /// caller guarantees each `NodeId` is unique per commit. Gate 0c's
    /// finalize step 6 dedups defensively as part of bucket compaction.
    ///
    /// Accepting the bucketing at commit-site keeps the code path
    /// `O(1)` amortised per node.
    pub fn record_node(&mut self, file: FileId, node: NodeId) {
        self.per_file_nodes.entry(file).or_default().push(node);
    }

    /// Remove `file`'s bucket and return its `NodeId`s, or an empty
    /// `Vec` if no bucket was present.
    ///
    /// Used by `RebuildGraph::remove_file` (Task 4 Step 2, scheduled
    /// after Gate 0c) to populate the tombstone set when a file is
    /// deleted from the workspace. The bucket is removed, not cleared,
    /// so the map shape matches the set of currently-live files.
    pub fn take_nodes(&mut self, file: FileId) -> Vec<NodeId> {
        self.per_file_nodes.remove(&file).unwrap_or_default()
    }

    /// Borrow `file`'s node bucket without removing it. Returns an
    /// empty slice when no bucket is present.
    #[must_use]
    pub fn nodes_for_file(&self, file: FileId) -> &[NodeId] {
        self.per_file_nodes
            .get(&file)
            .map_or(&[] as &[NodeId], Vec::as_slice)
    }

    /// Number of per-file buckets currently tracked.
    #[must_use]
    pub fn per_file_bucket_count(&self) -> usize {
        self.per_file_nodes.len()
    }

    /// Total number of `NodeId`s summed across every per-file bucket.
    ///
    /// Used by Gate 0c tests and the bucket-bijection assertion to
    /// sanity-check totals against the live arena count.
    #[must_use]
    pub fn total_recorded_nodes(&self) -> usize {
        self.per_file_nodes.values().map(Vec::len).sum()
    }

    /// Rewrite every `Option<StringId>` stored in `FileEntry::source_uri`
    /// through `remap`. Used by Gate 0c finalize step 1 after the
    /// interner's dedup pass.
    ///
    /// Empty `remap` is a no-op; entries whose `source_uri` is `None`
    /// are left alone.
    ///
    /// Live in the default build: the consumer is `RebuildGraph::finalize()`
    /// step 1, reached from the ungated public
    /// `build::incremental::incremental_rebuild` -> `finalize` path.
    pub(crate) fn rewrite_string_ids_through_remap(&mut self, remap: &HashMap<StringId, StringId>) {
        if remap.is_empty() {
            return;
        }
        for slot in self.entries.iter_mut().flatten() {
            if let Some(uri) = slot.source_uri
                && let Some(&canon) = remap.get(&uri)
            {
                slot.source_uri = Some(canon);
            }
        }
    }

    /// Apply `keep` to every `NodeId` in every per-file bucket, drop
    /// rejected IDs, dedup each bucket (preserving first-occurrence
    /// order), and drop any buckets that collapse to empty.
    ///
    /// This is the real impl behind the Gate 0c finalize step 6
    /// compaction. It runs *after* the arena's tombstone predicate has
    /// been fixed (step 2), so `keep` is backed by "arena has this
    /// `NodeId` live" semantics.
    ///
    /// Live in the default build: the consumer is the
    /// `NodeIdBearing::retain_nodes` impl driven by
    /// `RebuildGraph::finalize()` step 6, reached from the ungated public
    /// `build::incremental::incremental_rebuild` -> `finalize` path (the
    /// `rebuild::coverage` unit tests exercise it too).
    pub(crate) fn retain_nodes_in_buckets<F>(&mut self, keep: &F)
    where
        F: Fn(NodeId) -> bool + ?Sized,
    {
        self.per_file_nodes.retain(|_file, bucket| {
            bucket.retain(|id| keep(*id));
            // Dedup (stable) while preserving insertion order.
            let mut seen: std::collections::HashSet<NodeId> =
                std::collections::HashSet::with_capacity(bucket.len());
            bucket.retain(|id| seen.insert(*id));
            !bucket.is_empty()
        });
    }

    /// Iterate every `NodeId` across every bucket. Used by the Gate 0b
    /// / 0c [`NodeIdBearing`] impl to audit tombstone residue.
    ///
    /// Duplicates across buckets are emitted as-is; the residue check
    /// uses set membership.
    ///
    /// [`NodeIdBearing`]: crate::graph::unified::rebuild::coverage::NodeIdBearing
    ///
    /// Live in the default build: the consumer is the
    /// `NodeIdBearing::all_node_ids` impl driven by the debug-build
    /// residue check, reached from the ungated public
    /// `build::incremental::incremental_rebuild` -> `finalize` path (the
    /// `rebuild::coverage` unit tests exercise it too).
    pub(crate) fn iter_all_bucket_node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.per_file_nodes.values().flat_map(|v| v.iter().copied())
    }

    /// Returns the total number of allocated slots (including vacant/recycled
    /// ones and the sentinel at index 0).
    ///
    /// This is the length of the underlying `entries` vector, not the count of
    /// live files. Use this to iterate every possible `FileId` index.
    #[inline]
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.entries.len()
    }

    /// Registers a file path and returns its `FileId`.
    ///
    /// The path is normalized using best-effort canonicalization before registration.
    /// If the canonical path was already registered, returns the existing ID.
    ///
    /// # Best-Effort Normalization
    ///
    /// This method uses fallback behavior when canonicalization fails:
    /// 1. Tries full canonicalization (resolve symlinks, make absolute)
    /// 2. Falls back to converting relative path to absolute using current directory
    /// 3. Falls back to using the path as-is
    ///
    /// This means non-existent or inaccessible paths are still registered, but
    /// may not be truly canonical. Use [`try_register_strict`] if you need to
    /// guarantee canonicalization succeeds.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry has
    /// exhausted all available IDs (> 2^32 - 2 files).
    ///
    /// [`try_register_strict`]: Self::try_register_strict
    pub fn register(&mut self, path: &Path) -> Result<FileId, RegistryError> {
        self.register_with_language(path, None)
    }

    /// Registers a file path with an associated language.
    ///
    /// Similar to [`register`], but allows specifying the file's language.
    /// The language can be updated later using [`set_language`].
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry has
    /// exhausted all available IDs (> 2^32 - 2 files).
    ///
    /// [`register`]: Self::register
    /// [`set_language`]: Self::set_language
    pub fn register_with_language(
        &mut self,
        path: &Path,
        language: Option<Language>,
    ) -> Result<FileId, RegistryError> {
        // Normalize the path
        let canonical = Self::normalize_path(path);
        let arc_path: Arc<Path> = Arc::from(canonical.as_path());

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            // Update language if provided and entry exists
            if let Some(lang) = language
                && let Some(Some(entry)) = self.entries.get_mut(index as usize)
            {
                entry.language = Some(lang);
            }
            return Ok(FileId::new(index));
        }

        // Create new file entry
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language,
            is_external: false,
            content_hash: default_content_hash(),
            indexed_at: 0,
            source_uri: None,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            // Reuse a recycled slot
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            // Append new slot
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Registers a file path with strict canonicalization.
    ///
    /// Unlike [`register`], this method returns an error if the path cannot
    /// be canonicalized. Use this when you need to guarantee that all registered
    /// paths are truly canonical.
    ///
    /// # Errors
    ///
    /// - [`RegistryError::CanonicalizationFailed`]: Path cannot be canonicalized
    ///   (e.g., file doesn't exist, permission denied, or symlink loop).
    /// - [`RegistryError::CapacityExhausted`]: Registry capacity exhausted.
    ///
    /// [`register`]: Self::register
    pub fn try_register_strict(&mut self, path: &Path) -> Result<FileId, RegistryError> {
        // Require successful canonicalization
        let canonical = path
            .canonicalize()
            .map_err(|e| RegistryError::CanonicalizationFailed {
                path: path.to_path_buf(),
                message: e.to_string(),
            })?;

        let arc_path: Arc<Path> = Arc::from(canonical.as_path());

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            return Ok(FileId::new(index));
        }

        // Create new file entry without language
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language: None,
            is_external: false,
            content_hash: default_content_hash(),
            indexed_at: 0,
            source_uri: None,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Registers a path without normalization (for already-canonical paths).
    ///
    /// Use this when you know the path is already canonical (e.g., from
    /// file system enumeration).
    ///
    /// # Errors
    ///
    /// Returns an error if the registry is at capacity.
    pub fn register_canonical(&mut self, path: &Path) -> Result<FileId, RegistryError> {
        self.register_canonical_with_language(path, None)
    }

    /// Registers a canonical path with an associated language.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry is at capacity.
    pub fn register_canonical_with_language(
        &mut self,
        path: &Path,
        language: Option<Language>,
    ) -> Result<FileId, RegistryError> {
        let arc_path: Arc<Path> = Arc::from(path);

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            // Update language if provided and entry exists
            if let Some(lang) = language
                && let Some(Some(entry)) = self.entries.get_mut(index as usize)
            {
                entry.language = Some(lang);
            }
            return Ok(FileId::new(index));
        }

        // Create new file entry
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language,
            is_external: false,
            content_hash: default_content_hash(),
            indexed_at: 0,
            source_uri: None,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Resolves a `FileId` to its path.
    ///
    /// Returns `None` if the ID is invalid or has been unregistered.
    #[must_use]
    pub fn resolve(&self, id: FileId) -> Option<Arc<Path>> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        self.entries
            .get(index)
            .and_then(|opt| opt.as_ref().map(|entry| Arc::clone(&entry.path)))
    }

    /// Gets the language for a file.
    ///
    /// Returns `None` if the file ID is invalid, the file was unregistered,
    /// or no language has been set for this file.
    #[must_use]
    pub fn language_for_file(&self, file_id: FileId) -> Option<Language> {
        if file_id.is_invalid() {
            return None;
        }

        let index = file_id.index() as usize;
        self.entries
            .get(index)
            .and_then(|opt| opt.as_ref())
            .and_then(|entry| entry.language)
    }

    /// Sets or updates the language for a file.
    ///
    /// Returns `true` if the language was set successfully, `false` if the
    /// file ID is invalid or the file was unregistered.
    pub fn set_language(&mut self, file_id: FileId, language: Language) -> bool {
        if file_id.is_invalid() {
            return false;
        }

        let index = file_id.index() as usize;
        if let Some(Some(entry)) = self.entries.get_mut(index) {
            entry.language = Some(language);
            true
        } else {
            false
        }
    }

    /// Returns whether a file is external (e.g., from a classpath JAR).
    ///
    /// Returns `false` if the file ID is invalid or the file was unregistered.
    #[must_use]
    pub fn is_external(&self, id: FileId) -> bool {
        if id.is_invalid() {
            return false;
        }

        let index = id.index() as usize;
        self.entries
            .get(index)
            .and_then(|opt| opt.as_ref())
            .is_some_and(|entry| entry.is_external)
    }

    /// Registers an external file path and returns its `FileId`.
    ///
    /// External files originate from outside the project (e.g., classpath JARs).
    /// They are marked with `is_external = true` for filtering in queries and
    /// visualizations.
    ///
    /// The path is stored as-is (no normalization), since external files may
    /// reference virtual paths within JAR archives.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry has
    /// exhausted all available IDs.
    pub fn register_external(
        &mut self,
        path: impl AsRef<Path>,
        language: Option<Language>,
    ) -> Result<FileId, RegistryError> {
        self.register_external_with_uri(path, language, None)
    }

    /// Registers an external file path with an associated language and source URI.
    ///
    /// Like [`register_external`], but also stamps the interned source URI at
    /// registration time when one is available. For classpath entries,
    /// `source_uri` should carry the JAR origin (e.g.
    /// `jar:file:///…/rt.jar!/java/lang/Object.class`).
    ///
    /// # Invariant
    ///
    /// Callers that cannot provide a URI yet should use [`register_external`]
    /// and stamp via [`set_source_uri`] later. This method does not assert on
    /// `source_uri` and accepts `None` for the two-phase registration flow.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry has
    /// exhausted all available IDs.
    ///
    /// [`register_external`]: Self::register_external
    /// [`set_source_uri`]: Self::set_source_uri
    pub fn register_external_with_uri(
        &mut self,
        path: impl AsRef<Path>,
        language: Option<Language>,
        source_uri: Option<StringId>,
    ) -> Result<FileId, RegistryError> {
        let path = path.as_ref();
        let arc_path: Arc<Path> = Arc::from(path);

        // Check if already registered
        if let Some(&index) = self.lookup.get(&arc_path) {
            // Update external flag, language, and source_uri if entry exists
            if let Some(Some(entry)) = self.entries.get_mut(index as usize) {
                entry.is_external = true;
                if let Some(lang) = language {
                    entry.language = Some(lang);
                }
                if source_uri.is_some() {
                    entry.source_uri = source_uri;
                }
            }
            return Ok(FileId::new(index));
        }

        // Create new external file entry
        let entry = FileEntry {
            path: Arc::clone(&arc_path),
            language,
            is_external: true,
            content_hash: default_content_hash(),
            indexed_at: 0,
            source_uri,
        };

        // Allocate new slot
        let index = if let Some(free_idx) = self.free_list.pop() {
            self.entries[free_idx as usize] = Some(entry);
            free_idx
        } else {
            let idx = self.entries.len();
            if idx > u32::MAX as usize - 1 {
                return Err(RegistryError::CapacityExhausted);
            }
            self.entries.push(Some(entry));
            u32::try_from(idx).map_err(|_| RegistryError::CapacityExhausted)?
        };

        self.lookup.insert(arc_path, index);
        Ok(FileId::new(index))
    }

    /// Gets all files that match the specified language.
    ///
    /// Returns a vector of `(FileId, Arc<Path>)` pairs for all files
    /// that have been assigned the given language.
    #[must_use]
    pub fn files_by_language(&self, language: Language) -> Vec<(FileId, Arc<Path>)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                opt.as_ref().and_then(|entry| {
                    if entry.language == Some(language) {
                        let idx_u32 = u32::try_from(idx).ok()?;
                        Some((FileId::new(idx_u32), Arc::clone(&entry.path)))
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    /// Unregisters a file, freeing its slot for reuse.
    ///
    /// Returns the path that was unregistered, or `None` if the ID was invalid.
    pub fn unregister(&mut self, id: FileId) -> Option<Arc<Path>> {
        if id.is_invalid() {
            return None;
        }

        let index = id.index() as usize;
        if index >= self.entries.len() {
            return None;
        }

        if let Some(entry) = self.entries[index].take() {
            self.lookup.remove(&entry.path);
            if let Ok(index_u32) = u32::try_from(index) {
                self.free_list.push(index_u32);
                self.per_file_nodes.remove(&FileId::new(index_u32));
            } else {
                log::warn!("File registry index overflow when recycling slot {index}");
            }
            Some(entry.path)
        } else {
            None
        }
    }

    /// Checks if a path is registered.
    #[must_use]
    pub fn contains(&self, path: &Path) -> bool {
        let canonical = Self::normalize_path(path);
        self.lookup.contains_key(canonical.as_path())
    }

    /// Checks if a path is registered without normalization.
    #[must_use]
    pub fn contains_canonical(&self, path: &Path) -> bool {
        self.lookup.contains_key(path)
    }

    /// Gets the `FileId` for a path if it's registered.
    ///
    /// Unlike `register()`, this does not create a new entry.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<FileId> {
        let canonical = Self::normalize_path(path);
        self.lookup
            .get(canonical.as_path())
            .map(|&idx| FileId::new(idx))
    }

    /// Gets the `FileId` for a canonical path if it's registered.
    #[must_use]
    pub fn get_canonical(&self, path: &Path) -> Option<FileId> {
        self.lookup.get(path).map(|&idx| FileId::new(idx))
    }

    /// Returns an iterator over all registered files with their IDs.
    pub fn iter(&self) -> impl Iterator<Item = (FileId, &Arc<Path>)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                opt.as_ref().and_then(|entry| {
                    u32::try_from(idx)
                        .ok()
                        .map(|idx_u32| (FileId::new(idx_u32), &entry.path))
                })
            })
    }

    /// Returns an iterator over all registered files with their IDs and languages.
    pub fn iter_with_language(
        &self,
    ) -> impl Iterator<Item = (FileId, &Arc<Path>, Option<Language>)> {
        self.entries
            .iter()
            .enumerate()
            .skip(1) // Skip INVALID slot
            .filter_map(|(idx, opt)| {
                opt.as_ref().and_then(|entry| {
                    u32::try_from(idx)
                        .ok()
                        .map(|idx_u32| (FileId::new(idx_u32), &entry.path, entry.language))
                })
            })
    }

    /// Registers multiple file paths in a single batch operation.
    ///
    /// Each file is registered with its optional language using
    /// [`register_with_language`]. Duplicate paths within the batch (or
    /// already registered) receive the same `FileId`, matching the
    /// existing deduplication behavior.
    ///
    /// # Errors
    ///
    /// Returns `RegistryError::CapacityExhausted` if the registry
    /// exhausts all available IDs during the batch.  On error, files
    /// registered before the failure remain in the registry.
    ///
    /// [`register_with_language`]: Self::register_with_language
    pub fn register_batch(
        &mut self,
        files: &[(PathBuf, Option<Language>)],
    ) -> Result<Vec<FileId>, RegistryError> {
        let mut ids = Vec::with_capacity(files.len());
        for (path, language) in files {
            let id = self.register_with_language(path, *language)?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Clears all registered files.
    pub fn clear(&mut self) {
        self.entries.truncate(1); // Keep INVALID slot
        self.entries[0] = None;
        self.lookup.clear();
        self.free_list.clear();
        self.per_file_nodes.clear();
    }

    /// Reserves capacity for at least `additional` more files.
    pub fn reserve(&mut self, additional: usize) {
        self.entries.reserve(additional);
        self.lookup.reserve(additional);
    }

    // ------------------------------------------------------------------
    // Phase 1 fact-layer provenance accessors and setters (P1U05).
    // ------------------------------------------------------------------

    /// Returns a borrowed provenance view for `id`.
    ///
    /// Returns `None` for the invalid sentinel, unregistered IDs, and
    /// recycled (vacant) slots.
    #[must_use]
    pub fn file_provenance(&self, id: FileId) -> Option<FileProvenanceView<'_>> {
        if id.is_invalid() {
            return None;
        }
        let entry = self.entries.get(id.index() as usize)?.as_ref()?;
        Some(FileProvenanceView {
            content_hash: &entry.content_hash,
            indexed_at: entry.indexed_at,
            source_uri: entry.source_uri,
            is_external: entry.is_external,
        })
    }

    /// Stamps the content hash on a registered file entry.
    ///
    /// Intended for use by the build pipeline (P1U08) after the file bytes
    /// have been read for tree-sitter but before the extraction pass starts.
    /// Returns `false` if the ID is invalid or the slot is vacant.
    pub fn set_content_hash(&mut self, id: FileId, hash: [u8; 32]) -> bool {
        if id.is_invalid() {
            return false;
        }
        if let Some(Some(entry)) = self.entries.get_mut(id.index() as usize) {
            entry.content_hash = hash;
            true
        } else {
            false
        }
    }

    /// Stamps the indexed-at epoch on a registered file entry.
    ///
    /// In Phase 1, every `FileEntry` in a single build shares the same
    /// `indexed_at` value, equal to the snapshot's `fact_epoch`.
    /// Returns `false` if the ID is invalid or the slot is vacant.
    pub fn set_indexed_at(&mut self, id: FileId, epoch: u64) -> bool {
        if id.is_invalid() {
            return false;
        }
        if let Some(Some(entry)) = self.entries.get_mut(id.index() as usize) {
            entry.indexed_at = epoch;
            true
        } else {
            false
        }
    }

    /// Stamps the interned source URI on a registered file entry.
    ///
    /// Should only be called for external or synthetic files. The caller is
    /// responsible for interning the URI through the `StringInterner` before
    /// passing the resulting `StringId` here.
    ///
    /// # Debug assertion
    ///
    /// In debug builds, asserts that if `is_external` is `true` on this
    /// entry and `source_uri` is `Some`, the invariant `is_external ⇒
    /// source_uri.is_some()` was already met or is met by this call. This
    /// catches the converse case: calling `set_source_uri(None)` on an
    /// external entry would violate the invariant.
    ///
    /// Returns `false` if the ID is invalid or the slot is vacant.
    pub fn set_source_uri(&mut self, id: FileId, uri: Option<StringId>) -> bool {
        if id.is_invalid() {
            return false;
        }
        if let Some(Some(entry)) = self.entries.get_mut(id.index() as usize) {
            debug_assert!(
                !(entry.is_external && uri.is_none()),
                "set_source_uri(None) on an external file violates is_external => source_uri.is_some()"
            );
            entry.source_uri = uri;
            true
        } else {
            false
        }
    }

    /// Returns statistics about the registry.
    #[must_use]
    pub fn stats(&self) -> RegistryStats {
        RegistryStats {
            file_count: self.len(),
            free_slots: self.free_list.len(),
            capacity: self.entries.capacity(),
        }
    }

    /// Normalizes a path for consistent lookup.
    ///
    /// This performs best-effort canonicalization:
    /// - Tries to canonicalize (resolve symlinks, absolute path)
    /// - Falls back to converting to absolute path
    /// - Falls back to using the path as-is if it's already absolute
    fn normalize_path(path: &Path) -> PathBuf {
        // Try full canonicalization first
        if let Ok(canonical) = path.canonicalize() {
            return canonical;
        }

        // Fall back to just making it absolute
        if path.is_relative()
            && let Ok(cwd) = std::env::current_dir()
        {
            return cwd.join(path);
        }

        // Last resort: return as-is (converted to PathBuf)
        path.to_path_buf()
    }
}

impl Default for FileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for FileRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FileRegistry(files={}, free={})",
            self.len(),
            self.free_list.len()
        )
    }
}

/// Statistics about a `FileRegistry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryStats {
    /// Number of registered files.
    pub file_count: usize,
    /// Number of free (recyclable) slots.
    pub free_slots: usize,
    /// Allocated capacity.
    pub capacity: usize,
}

impl crate::graph::unified::memory::GraphMemorySize for FileRegistry {
    fn heap_bytes(&self) -> usize {
        use crate::graph::unified::memory::HASHMAP_ENTRY_OVERHEAD;

        let entries_vec = self.entries.capacity() * std::mem::size_of::<Option<FileEntry>>();
        let lookup = self.lookup.capacity()
            * (std::mem::size_of::<Arc<Path>>()
                + std::mem::size_of::<u32>()
                + HASHMAP_ENTRY_OVERHEAD);
        let free_list = self.free_list.capacity() * std::mem::size_of::<u32>();
        let per_file_map = self.per_file_nodes.capacity()
            * (std::mem::size_of::<FileId>()
                + std::mem::size_of::<Vec<NodeId>>()
                + HASHMAP_ENTRY_OVERHEAD);
        let per_file_buckets: usize = self
            .per_file_nodes
            .values()
            .map(|v| v.capacity() * std::mem::size_of::<NodeId>())
            .sum();
        entries_vec + lookup + free_list + per_file_map + per_file_buckets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_new() {
        let registry = FileRegistry::new();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_with_capacity() {
        let registry = FileRegistry::with_capacity(100);
        assert_eq!(registry.len(), 0);
        assert!(registry.entries.capacity() >= 101); // +1 for INVALID slot
    }

    #[test]
    fn test_register_and_resolve() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let id = registry.register(&file_path).unwrap();

        assert!(!id.is_invalid());
        assert_eq!(registry.len(), 1);

        let resolved = registry.resolve(id).unwrap();
        // Both should resolve to the same canonical path
        assert_eq!(resolved.canonicalize().ok(), file_path.canonicalize().ok());
    }

    #[test]
    fn test_register_duplicate() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        let id1 = registry.register(&file_path).unwrap();
        let id2 = registry.register(&file_path).unwrap();

        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_register_different() {
        let tmp = TempDir::new().unwrap();
        let file1 = tmp.path().join("a.rs");
        let file2 = tmp.path().join("b.rs");
        fs::write(&file1, "").unwrap();
        fs::write(&file2, "").unwrap();

        let mut registry = FileRegistry::new();
        let id1 = registry.register(&file1).unwrap();
        let id2 = registry.register(&file2).unwrap();

        assert_ne!(id1, id2);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn test_register_canonical() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/canonical/path/file.rs");
        let id = registry.register_canonical(path).unwrap();

        assert!(!id.is_invalid());
        assert_eq!(registry.resolve(id).unwrap().as_ref(), path);
    }

    #[test]
    fn test_resolve_invalid() {
        let registry = FileRegistry::new();
        assert!(registry.resolve(FileId::INVALID).is_none());
    }

    #[test]
    fn test_resolve_out_of_bounds() {
        let registry = FileRegistry::new();
        assert!(registry.resolve(FileId::new(999)).is_none());
    }

    #[test]
    fn test_unregister() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/file.rs");
        let id = registry.register_canonical(path).unwrap();

        assert_eq!(registry.len(), 1);

        let removed = registry.unregister(id);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
        assert!(registry.resolve(id).is_none());
    }

    #[test]
    fn test_unregister_invalid() {
        let mut registry = FileRegistry::new();
        assert!(registry.unregister(FileId::INVALID).is_none());
    }

    #[test]
    fn test_free_list_reuse() {
        let mut registry = FileRegistry::new();
        let path1 = Path::new("/test/a.rs");
        let path2 = Path::new("/test/b.rs");

        let id1 = registry.register_canonical(path1).unwrap();
        registry.unregister(id1);

        let id2 = registry.register_canonical(path2).unwrap();
        assert_eq!(id1.index(), id2.index()); // Same slot reused
    }

    #[test]
    fn test_contains() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        registry.register(&file_path).unwrap();

        assert!(registry.contains(&file_path));
        assert!(!registry.contains(Path::new("/nonexistent/path.rs")));
    }

    #[test]
    fn test_contains_canonical() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/canonical/test.rs");
        registry.register_canonical(path).unwrap();

        assert!(registry.contains_canonical(path));
        assert!(!registry.contains_canonical(Path::new("/other/path.rs")));
    }

    #[test]
    fn test_get() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        let id = registry.register(&file_path).unwrap();

        assert_eq!(registry.get(&file_path), Some(id));
        assert_eq!(registry.get(Path::new("/nonexistent.rs")), None);
    }

    #[test]
    fn test_get_canonical() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/canonical/test.rs");
        let id = registry.register_canonical(path).unwrap();

        assert_eq!(registry.get_canonical(path), Some(id));
        assert_eq!(registry.get_canonical(Path::new("/other.rs")), None);
    }

    #[test]
    fn test_iter() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/a.rs")).unwrap();
        registry.register_canonical(Path::new("/b.rs")).unwrap();
        registry.register_canonical(Path::new("/c.rs")).unwrap();

        let paths: Vec<_> = registry.iter().map(|(_, p)| p.to_path_buf()).collect();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&PathBuf::from("/a.rs")));
        assert!(paths.contains(&PathBuf::from("/b.rs")));
        assert!(paths.contains(&PathBuf::from("/c.rs")));
    }

    #[test]
    fn test_clear() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/a.rs")).unwrap();
        registry.register_canonical(Path::new("/b.rs")).unwrap();

        assert_eq!(registry.len(), 2);
        registry.clear();
        assert_eq!(registry.len(), 0);
        assert!(registry.is_empty());
    }

    #[test]
    fn test_reserve() {
        let mut registry = FileRegistry::new();
        registry.reserve(1000);
        assert!(registry.entries.capacity() >= 1001);
    }

    #[test]
    fn test_display() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/test.rs")).unwrap();

        let display = format!("{registry}");
        assert!(display.contains("FileRegistry"));
        assert!(display.contains("files=1"));
    }

    #[test]
    fn test_stats() {
        let mut registry = FileRegistry::new();
        registry.register_canonical(Path::new("/a.rs")).unwrap();
        registry.register_canonical(Path::new("/b.rs")).unwrap();

        let stats = registry.stats();
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.free_slots, 0);
    }

    #[test]
    fn test_default() {
        let registry: FileRegistry = FileRegistry::default();
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_registry_error_display() {
        let err = RegistryError::CanonicalizationFailed {
            path: PathBuf::from("/test/path"),
            message: "not found".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("/test/path"));
        assert!(display.contains("not found"));

        let err2 = RegistryError::CapacityExhausted;
        let display2 = format!("{err2}");
        assert!(display2.contains("capacity exhausted"));
    }

    #[test]
    fn test_unicode_path() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/日本語/ファイル.rs");
        let id = registry.register_canonical(path).unwrap();

        let resolved = registry.resolve(id).unwrap();
        assert_eq!(resolved.as_ref(), path);
    }

    #[test]
    fn test_try_register_strict_success() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let id = registry.try_register_strict(&file_path).unwrap();

        assert!(!id.is_invalid());
        assert_eq!(registry.len(), 1);

        // Verify it returns canonical path
        let resolved = registry.resolve(id).unwrap();
        assert_eq!(resolved.as_ref(), file_path.canonicalize().unwrap());
    }

    #[test]
    fn test_try_register_strict_nonexistent() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/nonexistent/path/that/does/not/exist.rs");

        let result = registry.try_register_strict(path);
        assert!(result.is_err());

        match result.unwrap_err() {
            RegistryError::CanonicalizationFailed {
                path: err_path,
                message,
            } => {
                assert_eq!(err_path, path);
                assert!(!message.is_empty());
            }
            RegistryError::CapacityExhausted => {
                panic!("Expected CanonicalizationFailed, got CapacityExhausted")
            }
        }
    }

    #[test]
    fn test_register_fallback_nonexistent() {
        // Verify that register() uses fallback for non-existent paths
        let mut registry = FileRegistry::new();
        let path = Path::new("/nonexistent/path/file.rs");

        // register() should succeed with fallback behavior
        let result = registry.register(path);
        assert!(result.is_ok());

        let id = result.unwrap();
        let resolved = registry.resolve(id).unwrap();

        // The path should be stored (as-is since it can't be canonicalized)
        // and should contain the original path components
        assert!(resolved.to_string_lossy().contains("file.rs"));
    }

    #[test]
    fn test_try_register_strict_duplicate() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        fs::write(&file_path, "").unwrap();

        let mut registry = FileRegistry::new();
        let id1 = registry.try_register_strict(&file_path).unwrap();
        let id2 = registry.try_register_strict(&file_path).unwrap();

        assert_eq!(id1, id2);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_language_tracking_basic() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/file.rs");
        let id = registry.register_canonical(path).unwrap();

        // Initially no language
        assert_eq!(registry.language_for_file(id), None);

        // Set language
        assert!(registry.set_language(id, Language::Rust));
        assert_eq!(registry.language_for_file(id), Some(Language::Rust));

        // Update language
        assert!(registry.set_language(id, Language::JavaScript));
        assert_eq!(registry.language_for_file(id), Some(Language::JavaScript));
    }

    #[test]
    fn test_language_tracking_invalid_id() {
        let mut registry = FileRegistry::new();

        // Invalid ID should return None
        assert_eq!(registry.language_for_file(FileId::INVALID), None);

        // Setting language on invalid ID should fail
        assert!(!registry.set_language(FileId::INVALID, Language::Rust));
    }

    #[test]
    fn test_register_with_language() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/main.py");

        let id = registry
            .register_canonical_with_language(path, Some(Language::Python))
            .unwrap();

        assert_eq!(registry.language_for_file(id), Some(Language::Python));
        assert_eq!(registry.resolve(id).unwrap().as_ref(), path);
    }

    #[test]
    fn test_register_with_language_duplicate_updates() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/script.js");

        // Register with JavaScript
        let id1 = registry
            .register_canonical_with_language(path, Some(Language::JavaScript))
            .unwrap();
        assert_eq!(registry.language_for_file(id1), Some(Language::JavaScript));

        // Re-register same path with TypeScript updates language
        let id2 = registry
            .register_canonical_with_language(path, Some(Language::TypeScript))
            .unwrap();

        assert_eq!(id1, id2, "Should return same ID for duplicate path");
        assert_eq!(registry.language_for_file(id2), Some(Language::TypeScript));
    }

    #[test]
    fn test_files_by_language_empty() {
        let registry = FileRegistry::new();
        let files = registry.files_by_language(Language::Rust);
        assert!(files.is_empty());
    }

    #[test]
    fn test_files_by_language_single() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/src/main.rs");
        let id = registry
            .register_canonical_with_language(path, Some(Language::Rust))
            .unwrap();

        let files = registry.files_by_language(Language::Rust);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, id);
        assert_eq!(files[0].1.as_ref(), path);

        // Different language should return empty
        let js_files = registry.files_by_language(Language::JavaScript);
        assert!(js_files.is_empty());
    }

    #[test]
    fn test_files_by_language_multiple() {
        let mut registry = FileRegistry::new();

        // Register Rust files
        let rs1 = Path::new("/src/main.rs");
        let rs2 = Path::new("/src/lib.rs");
        let id1 = registry
            .register_canonical_with_language(rs1, Some(Language::Rust))
            .unwrap();
        let id2 = registry
            .register_canonical_with_language(rs2, Some(Language::Rust))
            .unwrap();

        // Register Python file
        let py1 = Path::new("/scripts/test.py");
        let id3 = registry
            .register_canonical_with_language(py1, Some(Language::Python))
            .unwrap();

        // Register file without language
        let _ = registry
            .register_canonical(Path::new("/data/config.json"))
            .unwrap();

        // Query Rust files
        let rust_files = registry.files_by_language(Language::Rust);
        assert_eq!(rust_files.len(), 2);
        let rust_ids: Vec<_> = rust_files.iter().map(|(id, _)| *id).collect();
        assert!(rust_ids.contains(&id1));
        assert!(rust_ids.contains(&id2));

        // Query Python files
        let python_files = registry.files_by_language(Language::Python);
        assert_eq!(python_files.len(), 1);
        assert_eq!(python_files[0].0, id3);

        // Query JavaScript files (none registered)
        let js_files = registry.files_by_language(Language::JavaScript);
        assert!(js_files.is_empty());
    }

    #[test]
    fn test_iter_with_language() {
        let mut registry = FileRegistry::new();

        let rs_path = Path::new("/src/main.rs");
        let py_path = Path::new("/scripts/test.py");
        let no_lang_path = Path::new("/config.json");

        let id1 = registry
            .register_canonical_with_language(rs_path, Some(Language::Rust))
            .unwrap();
        let id2 = registry
            .register_canonical_with_language(py_path, Some(Language::Python))
            .unwrap();
        let id3 = registry.register_canonical(no_lang_path).unwrap();

        let entries: Vec<_> = registry.iter_with_language().collect();
        assert_eq!(entries.len(), 3);

        // Find each entry and verify
        let rs_entry = entries.iter().find(|(id, _, _)| *id == id1).unwrap();
        assert_eq!(rs_entry.1.as_ref(), rs_path);
        assert_eq!(rs_entry.2, Some(Language::Rust));

        let py_entry = entries.iter().find(|(id, _, _)| *id == id2).unwrap();
        assert_eq!(py_entry.1.as_ref(), py_path);
        assert_eq!(py_entry.2, Some(Language::Python));

        let no_lang_entry = entries.iter().find(|(id, _, _)| *id == id3).unwrap();
        assert_eq!(no_lang_entry.1.as_ref(), no_lang_path);
        assert_eq!(no_lang_entry.2, None);
    }

    #[test]
    fn test_unregister_with_language() {
        let mut registry = FileRegistry::new();
        let path = Path::new("/test/file.rs");
        let id = registry
            .register_canonical_with_language(path, Some(Language::Rust))
            .unwrap();

        assert_eq!(registry.language_for_file(id), Some(Language::Rust));
        assert_eq!(registry.files_by_language(Language::Rust).len(), 1);

        // Unregister
        let removed = registry.unregister(id);
        assert!(removed.is_some());

        // Language should be gone
        assert_eq!(registry.language_for_file(id), None);
        assert_eq!(registry.files_by_language(Language::Rust).len(), 0);
    }

    #[test]
    fn test_language_serialization() {
        let mut registry = FileRegistry::new();
        let path1 = Path::new("/src/main.rs");
        let path2 = Path::new("/src/lib.py");

        registry
            .register_canonical_with_language(path1, Some(Language::Rust))
            .unwrap();
        registry
            .register_canonical_with_language(path2, Some(Language::Python))
            .unwrap();

        // Serialize to JSON
        let json = serde_json::to_string(&registry).unwrap();

        // Deserialize
        let deserialized: FileRegistry = serde_json::from_str(&json).unwrap();

        // Verify languages are preserved
        let rust_files = deserialized.files_by_language(Language::Rust);
        assert_eq!(rust_files.len(), 1);

        let python_files = deserialized.files_by_language(Language::Python);
        assert_eq!(python_files.len(), 1);
    }

    #[test]
    fn test_language_with_best_effort_register() {
        let mut registry = FileRegistry::new();
        // Non-existent path (uses fallback normalization)
        let path = Path::new("/nonexistent/file.rs");

        let id = registry
            .register_with_language(path, Some(Language::Rust))
            .unwrap();

        assert_eq!(registry.language_for_file(id), Some(Language::Rust));
    }

    #[test]
    #[allow(clippy::similar_names)] // file1/file2/file3 vs files: intentional test variable names
    fn test_register_batch() {
        let tmp = TempDir::new().unwrap();
        let file1 = tmp.path().join("alpha.rs");
        let file2 = tmp.path().join("beta.py");
        let file3 = tmp.path().join("gamma.js");
        fs::write(&file1, "fn main() {}").unwrap();
        fs::write(&file2, "print('hello')").unwrap();
        fs::write(&file3, "console.log('hi')").unwrap();

        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![
            (file1.clone(), Some(Language::Rust)),
            (file2.clone(), Some(Language::Python)),
            (file3.clone(), Some(Language::JavaScript)),
        ];

        let ids = registry.register_batch(&files).unwrap();

        // Should return 3 unique IDs
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);
        assert_ne!(ids[0], ids[2]);

        // All IDs should be resolvable
        for (i, id) in ids.iter().enumerate() {
            let resolved = registry.resolve(*id).unwrap();
            let expected_canonical = files[i].0.canonicalize().unwrap();
            assert_eq!(resolved.as_ref(), expected_canonical.as_path());
        }

        // Languages should be set
        assert_eq!(registry.language_for_file(ids[0]), Some(Language::Rust));
        assert_eq!(registry.language_for_file(ids[1]), Some(Language::Python));
        assert_eq!(
            registry.language_for_file(ids[2]),
            Some(Language::JavaScript)
        );

        // Registry should have 3 files
        assert_eq!(registry.len(), 3);
    }

    #[test]
    fn test_register_batch_empty() {
        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![];

        let ids = registry.register_batch(&files).unwrap();

        assert!(ids.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_register_batch_duplicate_paths() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("dup.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![
            (file.clone(), Some(Language::Rust)),
            (file.clone(), Some(Language::Rust)),
        ];

        let ids = registry.register_batch(&files).unwrap();

        // Duplicate path returns same FileId (deduplication behavior)
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], ids[1]);

        // Only 1 unique file registered
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn test_register_batch_duplicate_paths_different_languages() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("polyglot.rs");
        fs::write(&file, "fn main() {}").unwrap();

        let mut registry = FileRegistry::new();
        let files: Vec<(PathBuf, Option<Language>)> = vec![
            (file.clone(), Some(Language::Rust)),
            (file.clone(), Some(Language::Python)),
        ];

        let ids = registry.register_batch(&files).unwrap();

        // Same FileId for same path
        assert_eq!(ids[0], ids[1]);
        assert_eq!(registry.len(), 1);

        // Last language wins (matches register_with_language dedup behavior)
        assert_eq!(registry.language_for_file(ids[0]), Some(Language::Python));
    }

    // ------------------------------------------------------------------
    // Phase 1 P1U05: file provenance accessors
    // ------------------------------------------------------------------

    #[test]
    fn phase1_file_provenance_defaults_to_zero_hash_and_zero_epoch() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.rs");
        fs::write(&path, "fn main() {}").unwrap();

        let mut reg = FileRegistry::new();
        let id = reg.register(&path).unwrap();

        let view = reg.file_provenance(id).expect("provenance present");
        assert_eq!(view.content_hash, &[0u8; 32]);
        assert_eq!(view.indexed_at, 0);
        assert_eq!(view.source_uri, None);
        assert!(!view.is_external);
    }

    #[test]
    fn phase1_set_content_hash_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.rs");
        fs::write(&path, "fn main() {}").unwrap();

        let mut reg = FileRegistry::new();
        let id = reg.register(&path).unwrap();

        let hash = [0xAB; 32];
        assert!(reg.set_content_hash(id, hash));

        let view = reg.file_provenance(id).unwrap();
        assert_eq!(view.content_hash, &hash);
    }

    #[test]
    fn phase1_set_indexed_at_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.rs");
        fs::write(&path, "fn main() {}").unwrap();

        let mut reg = FileRegistry::new();
        let id = reg.register(&path).unwrap();
        assert!(reg.set_indexed_at(id, 42_000));

        let view = reg.file_provenance(id).unwrap();
        assert_eq!(view.indexed_at, 42_000);
    }

    #[test]
    fn phase1_set_source_uri_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.rs");
        fs::write(&path, "fn main() {}").unwrap();

        let mut reg = FileRegistry::new();
        let id = reg.register(&path).unwrap();
        let uri = StringId::new(7);
        assert!(reg.set_source_uri(id, Some(uri)));

        let view = reg.file_provenance(id).unwrap();
        assert_eq!(view.source_uri, Some(uri));
    }

    #[test]
    fn phase1_file_provenance_invalid_sentinel_returns_none() {
        let reg = FileRegistry::new();
        assert!(reg.file_provenance(FileId::INVALID).is_none());
    }

    #[test]
    fn phase1_file_provenance_survives_postcard_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.rs");
        fs::write(&path, "fn main() {}").unwrap();

        let mut reg = FileRegistry::new();
        let id = reg.register(&path).unwrap();
        reg.set_content_hash(id, [0xCD; 32]);
        reg.set_indexed_at(id, 12_345);
        reg.set_source_uri(id, Some(StringId::new(99)));

        let encoded = postcard::to_allocvec(&reg).expect("encode");
        let decoded: FileRegistry = postcard::from_bytes(&encoded).expect("decode");

        let view = decoded
            .file_provenance(id)
            .expect("provenance after decode");
        assert_eq!(view.content_hash, &[0xCD; 32]);
        assert_eq!(view.indexed_at, 12_345);
        assert_eq!(view.source_uri, Some(StringId::new(99)));
    }

    #[test]
    fn phase1_external_entry_exposes_is_external_true() {
        let mut reg = FileRegistry::new();
        let uri = StringId::new(42);
        let id = reg
            .register_external_with_uri("/virtual/path/Foo.class", Some(Language::Java), Some(uri))
            .unwrap();

        let view = reg.file_provenance(id).unwrap();
        assert!(view.is_external);
        assert_eq!(view.source_uri, Some(uri));
    }

    #[test]
    fn phase1_register_external_then_set_source_uri_round_trip() {
        let mut reg = FileRegistry::new();
        let id = reg
            .register_external("/virtual/path/Foo.class", Some(Language::Java))
            .unwrap();

        let initial_view = reg.file_provenance(id).unwrap();
        assert!(initial_view.is_external);
        assert_eq!(initial_view.source_uri, None);

        let uri = StringId::new(84);
        assert!(reg.set_source_uri(id, Some(uri)));

        let updated_view = reg.file_provenance(id).unwrap();
        assert!(updated_view.is_external);
        assert_eq!(updated_view.source_uri, Some(uri));
    }

    #[test]
    fn phase1_register_external_with_uri_accepts_none_and_records_externality() {
        let mut reg = FileRegistry::new();
        let id = reg
            .register_external_with_uri("/virtual/path/Foo.class", Some(Language::Java), None)
            .unwrap();

        let view = reg.file_provenance(id).unwrap();
        assert!(view.is_external);
        assert_eq!(view.source_uri, None);
    }

    #[test]
    fn phase1_setters_return_false_for_invalid_id() {
        let mut reg = FileRegistry::new();
        assert!(!reg.set_content_hash(FileId::INVALID, [0; 32]));
        assert!(!reg.set_indexed_at(FileId::INVALID, 1));
        assert!(!reg.set_source_uri(FileId::INVALID, None));
    }

    // ------------------------------------------------------------------
    // Per-file node buckets (K.B1 — Task 4 Step 1 pulled into Gate 0c).
    // ------------------------------------------------------------------

    #[test]
    fn file_registry_record_node_tracks_per_file_buckets() {
        let mut reg = FileRegistry::new();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        let n1 = NodeId::new(10, 1);
        let n2 = NodeId::new(11, 1);
        let n3 = NodeId::new(12, 1);

        reg.record_node(file_a, n1);
        reg.record_node(file_a, n2);
        reg.record_node(file_b, n3);

        assert_eq!(reg.per_file_bucket_count(), 2);
        assert_eq!(reg.total_recorded_nodes(), 3);
        assert_eq!(reg.nodes_for_file(file_a), &[n1, n2]);
        assert_eq!(reg.nodes_for_file(file_b), &[n3]);
        assert!(reg.nodes_for_file(FileId::new(99)).is_empty());
    }

    #[test]
    fn file_registry_take_nodes_drains_and_empties() {
        let mut reg = FileRegistry::new();
        let file = FileId::new(1);
        let n1 = NodeId::new(10, 1);
        let n2 = NodeId::new(11, 1);

        reg.record_node(file, n1);
        reg.record_node(file, n2);
        let drained = reg.take_nodes(file);
        assert_eq!(drained, vec![n1, n2]);
        assert!(reg.nodes_for_file(file).is_empty());
        assert_eq!(reg.per_file_bucket_count(), 0);
        // take from an empty bucket is well-defined.
        assert!(reg.take_nodes(file).is_empty());
    }

    #[test]
    fn file_registry_retain_nodes_in_buckets_drops_and_empties() {
        let mut reg = FileRegistry::new();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        let keep = NodeId::new(10, 1);
        let drop1 = NodeId::new(11, 1);
        let drop2 = NodeId::new(12, 1);
        reg.record_node(file_a, keep);
        reg.record_node(file_a, drop1);
        reg.record_node(file_b, drop2);

        reg.retain_nodes_in_buckets(&|id| id == keep);

        assert_eq!(reg.nodes_for_file(file_a), &[keep]);
        assert!(reg.nodes_for_file(file_b).is_empty());
        assert_eq!(reg.per_file_bucket_count(), 1, "empty bucket must drop");
        assert_eq!(reg.total_recorded_nodes(), 1);
    }

    #[test]
    fn file_registry_retain_nodes_dedups_within_bucket() {
        let mut reg = FileRegistry::new();
        let file = FileId::new(1);
        let n = NodeId::new(10, 1);
        // Intentionally record the same NodeId twice (simulating a
        // finalize scenario where parallel commit wrote the same id
        // into two chunks — defense-in-depth, not expected).
        reg.record_node(file, n);
        reg.record_node(file, n);
        reg.retain_nodes_in_buckets(&|_| true);
        assert_eq!(reg.nodes_for_file(file), &[n]);
    }

    #[test]
    fn file_registry_unregister_drops_bucket() {
        let mut reg = FileRegistry::new();
        let id = reg.register_canonical(Path::new("/a.rs")).unwrap();
        reg.record_node(id, NodeId::new(10, 1));
        reg.record_node(id, NodeId::new(11, 1));
        assert_eq!(reg.nodes_for_file(id).len(), 2);

        reg.unregister(id);
        assert!(reg.nodes_for_file(id).is_empty());
        assert_eq!(reg.per_file_bucket_count(), 0);
    }

    #[test]
    fn file_registry_clear_drops_buckets() {
        let mut reg = FileRegistry::new();
        let id = reg.register_canonical(Path::new("/a.rs")).unwrap();
        reg.record_node(id, NodeId::new(10, 1));
        reg.clear();
        assert_eq!(reg.per_file_bucket_count(), 0);
    }

    #[test]
    fn file_registry_rewrite_string_ids_updates_source_uri() {
        let mut reg = FileRegistry::new();
        let old_uri = StringId::new(7);
        let canon_uri = StringId::new(3);
        let id = reg
            .register_external_with_uri(
                "/virtual/path/Foo.class",
                Some(Language::Java),
                Some(old_uri),
            )
            .unwrap();
        let mut remap = HashMap::new();
        remap.insert(old_uri, canon_uri);
        reg.rewrite_string_ids_through_remap(&remap);
        let view = reg.file_provenance(id).unwrap();
        assert_eq!(view.source_uri, Some(canon_uri));
    }

    #[test]
    fn file_registry_rewrite_string_ids_is_noop_on_empty_remap() {
        let mut reg = FileRegistry::new();
        let uri = StringId::new(42);
        let id = reg
            .register_external_with_uri("/x.class", Some(Language::Java), Some(uri))
            .unwrap();
        reg.rewrite_string_ids_through_remap(&HashMap::new());
        assert_eq!(reg.file_provenance(id).unwrap().source_uri, Some(uri));
    }

    #[test]
    fn file_registry_per_file_nodes_for_gate0d_yields_all_buckets() {
        let mut reg = FileRegistry::new();
        let file_a = FileId::new(1);
        let file_b = FileId::new(2);
        let n1 = NodeId::new(10, 1);
        let n2 = NodeId::new(11, 1);
        let n3 = NodeId::new(12, 1);
        reg.record_node(file_a, n1);
        reg.record_node(file_a, n2);
        reg.record_node(file_b, n3);

        let collected: std::collections::BTreeMap<FileId, Vec<NodeId>> =
            reg.per_file_nodes_for_gate0d().collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(
            collected.get(&file_a).cloned().unwrap_or_default(),
            vec![n1, n2]
        );
        assert_eq!(
            collected.get(&file_b).cloned().unwrap_or_default(),
            vec![n3]
        );
    }
}
