//! Snapshot save/load implementation for the unified graph.
//!
//! This module provides functions to save and load graph snapshots
//! to/from disk using postcard serialization with length-prefixed framing.

use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Write};
use std::path::Path;
use tempfile::NamedTempFile;

use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use super::format::{
    FormatVersion, GraphHeader, MAGIC_BYTES_V9, MAGIC_BYTES_V10, MAGIC_BYTES_V11, MAGIC_BYTES_V12,
    MAGIC_BYTES_V13, VERSION,
};
use super::manifest::ConfigProvenance;
use crate::config::buffers::max_snapshot_bytes;
use crate::graph::unified::BidirectionalEdgeStore;
use crate::graph::unified::bind::alias::AliasTable;
use crate::graph::unified::bind::scope::arena::ScopeArena;
use crate::graph::unified::bind::scope::provenance::ScopeProvenanceStore;
use crate::graph::unified::bind::shadow::ShadowTable;
use crate::graph::unified::build::phase4e_binding::derive_binding_plane;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::resolution::is_canonical_graph_qualified_name;
use crate::graph::unified::storage::{
    AuxiliaryIndices, CIndirectSideTables, DispatchTables, EdgeProvenanceStore, FileRegistry,
    FileSegmentTable, FrameworkRoutesMap, NodeArena, NodeMetadataStore, NodeProvenanceStore,
    StringInterner,
};
use crate::plugin::PluginManager;

// Maximum snapshot data section size is resolved at runtime from
// `crate::config::buffers::max_snapshot_bytes()`, which honors the
// `SQRY_MAX_SNAPSHOT_BYTES` environment variable. The default is large enough
// to hold the serialized graph of very large codebases such as the Linux
// kernel (~7–8 GB). See `config::buffers` for the full contract.
//
// Historical note: prior to v8.0.x this file hardcoded a 2 GB constant which
// caused `sqry index` to fail on mega-repos like the Linux kernel. The limit
// now lives in the shared buffers-config module alongside the other
// `SQRY_*`-configurable limits, with a 1 GB–64 GB clamp range.

/// Maximum header size (1 MB).
const MAX_HEADER_BYTES: usize = 1_048_576;

/// Maximum reasonable node count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_NODES: usize = 100_000_000; // 100M nodes

/// Maximum reasonable edge count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_EDGES: usize = 1_000_000_000; // 1B edges

/// Maximum reasonable string count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_STRINGS: usize = 50_000_000; // 50M strings

/// Maximum reasonable file count (prevents allocation overflow on corrupt data)
const MAX_REASONABLE_FILES: usize = 1_000_000; // 1M files

/// Error type for persistence operations.
#[derive(Debug)]
pub enum PersistenceError {
    /// I/O error during read/write
    Io(std::io::Error),
    /// Serialization/deserialization error
    Serialization(String),
    /// Invalid magic bytes (not a sqry graph file)
    InvalidMagic {
        /// Expected magic bytes
        expected: Vec<u8>,
        /// Actual magic bytes found
        found: Vec<u8>,
    },
    /// Incompatible version
    IncompatibleVersion {
        /// Expected version number
        expected: u32,
        /// Actual version number found
        found: u32,
    },
    /// Plugin version mismatch (index built with different plugin versions)
    PluginVersionMismatch {
        /// Plugin ID with version mismatch
        plugin_id: String,
        /// Expected version (current)
        expected: String,
        /// Actual version found in index
        found: String,
    },
    /// Graph validation failed
    ValidationFailed(String),
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::Serialization(e) => write!(f, "Serialization error: {e}"),
            Self::InvalidMagic { expected, found } => {
                write!(
                    f,
                    "Invalid magic bytes: expected {expected:?}, found {found:?}. \
                     Index was created with an older version. Run `sqry index` to rebuild."
                )
            }
            Self::IncompatibleVersion { expected, found } => {
                write!(
                    f,
                    "Incompatible format version: expected {expected}, found {found}. \
                     Index was created with an older version. Run `sqry index` to rebuild."
                )
            }
            Self::PluginVersionMismatch {
                plugin_id,
                expected,
                found,
            } => {
                write!(
                    f,
                    "Plugin version mismatch for {plugin_id}: expected {expected}, found {found} (index needs rebuild)"
                )
            }
            Self::ValidationFailed(msg) => write!(f, "Validation failed: {msg}"),
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<std::io::Error> for PersistenceError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<postcard::Error> for PersistenceError {
    fn from(e: postcard::Error) -> Self {
        Self::Serialization(e.to_string())
    }
}

/// Serializable snapshot of the graph state (V8 shape, used as V8 legacy read target).
///
/// This is the V8 on-disk layout. The V9 writer extends this with Phase 2 binding-plane
/// stores. On load of a V8 blob, this struct is deserialized and then upconverted to V9
/// via `upconvert_v8_to_v9`.
#[derive(Debug, Serialize, Deserialize)]
struct GraphSnapshotData {
    /// Node storage
    nodes: NodeArena,
    /// Edge storage (forward + reverse)
    edges: BidirectionalEdgeStore,
    /// String interner
    strings: StringInterner,
    /// File registry
    files: FileRegistry,
    /// Auxiliary indices
    indices: AuxiliaryIndices,
    /// Sparse macro boundary metadata (keyed by full `NodeId`)
    macro_metadata: NodeMetadataStore,
    /// Dense node provenance (Phase 1 fact layer).
    node_provenance: NodeProvenanceStore,
    /// Dense edge provenance (Phase 1 fact layer).
    edge_provenance: EdgeProvenanceStore,
}

/// Serializable snapshot of the graph state (V9 — Phase 2 binding plane).
///
/// Extends [`GraphSnapshotData`] (V8) with `scope_arena`, `alias_table`,
/// `shadow_table`, and `scope_provenance`. On V9 load, the loader calls
/// `ScopeProvenanceStore::rebuild_reverse_index(&scope_arena)` after
/// deserialization to restore the in-memory `stable_id → ScopeId` map.
#[derive(Debug, Serialize, Deserialize)]
struct GraphSnapshotDataV9 {
    /// Node storage.
    nodes: NodeArena,
    /// Edge storage (forward + reverse).
    edges: BidirectionalEdgeStore,
    /// String interner.
    strings: StringInterner,
    /// File registry.
    files: FileRegistry,
    /// Auxiliary indices.
    indices: AuxiliaryIndices,
    /// Sparse macro boundary metadata (keyed by full `NodeId`).
    macro_metadata: NodeMetadataStore,
    /// Dense node provenance (Phase 1 fact layer).
    node_provenance: NodeProvenanceStore,
    /// Dense edge provenance (Phase 1 fact layer).
    edge_provenance: EdgeProvenanceStore,
    /// Phase 2: generational scope arena (slot-based, tombstoned-on-free).
    scope_arena: ScopeArena,
    /// Phase 2: alias derivation table (sorted by scope; `by_scope` index rebuilt on load).
    alias_table: AliasTable,
    /// Phase 2: shadow derivation table (sorted by scope+symbol; index rebuilt on load).
    shadow_table: ShadowTable,
    /// Phase 2: scope provenance store (reverse index rebuilt on load from `scope_arena`).
    scope_provenance: ScopeProvenanceStore,
}

/// Serializable snapshot of the graph state (V10 — Phase 3 derived DB).
///
/// Extends [`GraphSnapshotDataV9`] with `file_segments`. On V9 load, the
/// upconvert function rebuilds the segment table from the node arena.
///
/// # Wire-type pinning (U03 codex iter-1 BLOCKER fix)
///
/// `edges` and `macro_metadata` reference **versioned V10 mirror types**
/// in [`super::legacy_v10`] — *not* the live `BidirectionalEdgeStore` /
/// `NodeMetadataStore`. This pins the V10 postcard layout to pre-U02 /
/// pre-U04 shapes regardless of how those live types evolve. The V10 →
/// V11 upconvert ([`upconvert_v10_to_v11`]) translates these mirror
/// types into the live shapes.
#[derive(Debug, Serialize, Deserialize)]
struct GraphSnapshotDataV10 {
    /// Node storage.
    nodes: NodeArena,
    /// Edge storage (forward + reverse) — V10 wire-format mirror.
    edges: super::legacy_v10::BidirectionalEdgeStoreV10,
    /// String interner.
    strings: StringInterner,
    /// File registry.
    files: FileRegistry,
    /// Auxiliary indices.
    indices: AuxiliaryIndices,
    /// Sparse macro boundary metadata (keyed by full `NodeId`) — V10
    /// wire-format mirror (pre-U02 three-variant `NodeMetadata`).
    macro_metadata: super::legacy_v10::NodeMetadataStoreV10,
    /// Dense node provenance (Phase 1 fact layer).
    node_provenance: NodeProvenanceStore,
    /// Dense edge provenance (Phase 1 fact layer).
    edge_provenance: EdgeProvenanceStore,
    /// Phase 2: generational scope arena.
    scope_arena: ScopeArena,
    /// Phase 2: alias derivation table.
    alias_table: AliasTable,
    /// Phase 2: shadow derivation table.
    shadow_table: ShadowTable,
    /// Phase 2: scope provenance store.
    scope_provenance: ScopeProvenanceStore,
    /// Phase 3: per-file segment table mapping FileId to node arena ranges.
    file_segments: FileSegmentTable,
}

/// Serializable snapshot of the graph state (V11 — Phase A C-icall precision).
///
/// Extends [`GraphSnapshotDataV10`] with the
/// `c_indirect_tables: Option<CIndirectSideTables>` envelope slot. The slot
/// is gated behind `#[serde(default)]` so V10 snapshots that lack the field
/// decode it as `None` when piped through the V10 → V11 upconvert; V11
/// binaries that load a populated variant carry the side-table data
/// through to the reconstructed `CodeGraph`.
///
/// On V10 load, the loader calls [`upconvert_v10_to_v11`] which copies every
/// V10 field unchanged and sets `c_indirect_tables = None`. The metadata
/// store wire format (V11 = `StoredEntry { typed, flags }`) is already
/// emitted by `NodeMetadataStore::serialize` after U02, so V10 → V11 carries
/// no metadata-store reshape — the field is preserved by value.
///
/// **U09 retrofit**: the `c_indirect_tables` slot was previously typed as
/// the `Option<()>` placeholder; U09 swaps the placeholder for the live
/// [`CIndirectSideTables`] type. The serde shape was pinned during U03 so
/// the substitution is a pure type swap: `None` still postcard-encodes as
/// a single `0x00` discriminant byte (DESIGN §10.2 wire-shape contract,
/// covered by `storage::c_indirect::tests::option_none_serializes_to_one_byte`).
#[derive(Debug, Serialize, Deserialize)]
struct GraphSnapshotDataV11 {
    /// Node storage.
    nodes: NodeArena,
    /// Edge storage (forward + reverse).
    edges: BidirectionalEdgeStore,
    /// String interner.
    strings: StringInterner,
    /// File registry.
    files: FileRegistry,
    /// Auxiliary indices.
    indices: AuxiliaryIndices,
    /// Sparse metadata store keyed by full `NodeId`.
    ///
    /// After U02 the wire format is `Vec<NodeMetadataEntryV11>` —
    /// `StoredEntry { typed: Option<TypedMetadata>, flags: NodeFlags }`.
    macro_metadata: NodeMetadataStore,
    /// Dense node provenance (Phase 1 fact layer).
    node_provenance: NodeProvenanceStore,
    /// Dense edge provenance (Phase 1 fact layer).
    edge_provenance: EdgeProvenanceStore,
    /// Phase 2: generational scope arena.
    scope_arena: ScopeArena,
    /// Phase 2: alias derivation table.
    alias_table: AliasTable,
    /// Phase 2: shadow derivation table.
    shadow_table: ShadowTable,
    /// Phase 2: scope provenance store.
    scope_provenance: ScopeProvenanceStore,
    /// Phase 3: per-file segment table mapping FileId to node arena ranges.
    file_segments: FileSegmentTable,
    /// Phase A (U09): C indirect-call resolver side tables.
    ///
    /// `None` on non-C workspaces and on every V10 → V11 upconvert. When
    /// `Some(...)`, the inner [`CIndirectSideTables`] is round-tripped
    /// onto the reconstructed `CodeGraph` via
    /// `set_c_indirect_tables`. `#[serde(default)]` keeps the
    /// V10 → V11 upconvert path lossless: V10 snapshots that lack the
    /// field decode it as `None` (the postcard `0x00` discriminant byte;
    /// DESIGN §10.2).
    #[serde(default)]
    c_indirect_tables: Option<CIndirectSideTables>,
}

/// Serializable snapshot of the graph state (V12 — Phase β joint-stubs).
///
/// Extends [`GraphSnapshotDataV11`] with TWO new envelope slots that hold
/// the Phase β joint-stub side tables: Plan A's per-node framework-route
/// metadata and Plan B's dispatch-resolution tables. Both slots are
/// zero-initialised in this PR (no resolver populates them yet); the
/// downstream feature PRs wire the producers.
///
/// # Wire-shape contract
///
/// - `framework_routes_table` — flat `Vec<(NodeId, FrameworkRouteMetadata)>`
///   wire shape. We don't serialize the `BTreeMap` directly because postcard
///   does not natively support typed-key map encodings; the loader rebuilds
///   the `BTreeMap` on receipt. Empty `Vec` for non-extractor workspaces.
/// - `dispatch_tables_table` — postcard-serialized [`DispatchTables`].
///   Default (empty) for non-resolver workspaces.
///
/// Both fields carry `#[serde(default)]` so a V11 → V12 upconvert that
/// re-serializes legacy data without these slots — or a deserializer fed a
/// truncated V12 payload — decodes them as empty defaults (DESIGN §10.2
/// optional-slot wire contract).
#[derive(Debug, Serialize, Deserialize)]
struct GraphSnapshotDataV12 {
    /// Node storage.
    nodes: NodeArena,
    /// Edge storage (forward + reverse).
    edges: BidirectionalEdgeStore,
    /// String interner.
    strings: StringInterner,
    /// File registry.
    files: FileRegistry,
    /// Auxiliary indices.
    indices: AuxiliaryIndices,
    /// Sparse metadata store keyed by full `NodeId`.
    ///
    /// V12 inherits the V11 metadata-store wire shape unchanged — the new
    /// joint-stub fields ride the envelope slots below, not the metadata
    /// store's own serde format. This preserves bit-for-bit compatibility
    /// for the metadata-store payload across the V11 → V12 transition.
    macro_metadata: NodeMetadataStore,
    /// Dense node provenance (Phase 1 fact layer).
    node_provenance: NodeProvenanceStore,
    /// Dense edge provenance (Phase 1 fact layer).
    edge_provenance: EdgeProvenanceStore,
    /// Phase 2: generational scope arena.
    scope_arena: ScopeArena,
    /// Phase 2: alias derivation table.
    alias_table: AliasTable,
    /// Phase 2: shadow derivation table.
    shadow_table: ShadowTable,
    /// Phase 2: scope provenance store.
    scope_provenance: ScopeProvenanceStore,
    /// Phase 3: per-file segment table mapping FileId to node arena ranges.
    file_segments: FileSegmentTable,
    /// Phase A (U09): C indirect-call resolver side tables. Carried forward
    /// from V11 unchanged.
    #[serde(default)]
    c_indirect_tables: Option<CIndirectSideTables>,
    /// Plan A joint-stub (V12): per-node framework-route metadata. Empty
    /// for every V11 → V12 upconvert and for non-extractor workspaces.
    ///
    /// Wire shape: `Vec<(NodeId, FrameworkRouteMetadata)>`. The loader
    /// rebuilds the `BTreeMap` keyed on `NodeId` and reattaches it to the
    /// metadata store via `NodeMetadataStore::set_framework_routes`.
    #[serde(default)]
    framework_routes_table: Vec<(
        crate::graph::unified::node::id::NodeId,
        crate::graph::unified::storage::FrameworkRouteMetadata,
    )>,
    /// Plan B joint-stub (V12): per-snapshot dispatch-resolution tables.
    /// Empty (default) for every V11 → V12 upconvert and for non-resolver
    /// workspaces. Reattached to the metadata store via
    /// `NodeMetadataStore::set_dispatch_tables`.
    #[serde(default)]
    dispatch_tables_table: DispatchTables,
}

/// Serializable snapshot of the graph state (V13 — T3 error chains).
///
/// V13 is shape-identical to V12 on the wire — no fields are added or removed.
/// The version bump exists so that V12 readers reject snapshots whose
/// `EdgeKind` arena contains the T3 [`crate::graph::unified::edge::EdgeKind::Wraps`]
/// variant (added in T3 Cluster A) rather than silently failing to decode an
/// unknown discriminant.
///
/// Because the layout is byte-identical, this is a type alias rather than a
/// duplicated struct; the postcard wire format is therefore guaranteed-equal
/// by construction (no field-rename drift possible). The corresponding
/// upconvert ([`upconvert_v12_to_v13`]) is an identity-mapping function.
type GraphSnapshotDataV13 = GraphSnapshotDataV12;

/// V7-shaped snapshot data (pre-Phase-1, no provenance fields).
///
/// Used only by the V7 legacy read path to deserialize old blobs.
/// After deserialization, the loader converts to [`GraphSnapshotData`]
/// with empty provenance stores sized to the arena/CSR slot counts.
#[derive(Debug, Deserialize)]
struct GraphSnapshotDataV7 {
    nodes: NodeArena,
    edges: BidirectionalEdgeStore,
    strings: StringInterner,
    files: FileRegistry,
    indices: AuxiliaryIndices,
    macro_metadata: NodeMetadataStore,
}

/// Validate header counts to prevent allocation overflow on corrupted data.
///
/// This function checks that header counts are within reasonable bounds
/// to prevent memory allocation panics when postcard tries to deserialize
/// corrupted data with huge length fields.
fn validate_header_sanity(header: &GraphHeader) -> Result<(), PersistenceError> {
    if header.node_count > MAX_REASONABLE_NODES {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable node_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.node_count, MAX_REASONABLE_NODES
        )));
    }
    if header.edge_count > MAX_REASONABLE_EDGES {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable edge_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.edge_count, MAX_REASONABLE_EDGES
        )));
    }
    if header.string_count > MAX_REASONABLE_STRINGS {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable string_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.string_count, MAX_REASONABLE_STRINGS
        )));
    }
    if header.file_count > MAX_REASONABLE_FILES {
        return Err(PersistenceError::ValidationFailed(format!(
            "Unreasonable file_count: {} exceeds maximum of {}. \
             This likely indicates a corrupted snapshot file.",
            header.file_count, MAX_REASONABLE_FILES
        )));
    }
    Ok(())
}

/// Validates header counts against a deserialized V8 snapshot (legacy; kept for
/// reference — all live load paths now use [`validate_loaded_snapshot_v9`]).
#[allow(dead_code)]
fn validate_loaded_snapshot(
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotData,
) -> Result<(), PersistenceError> {
    let forward_stats = snapshot_data.edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;

    if header.node_count != snapshot_data.nodes.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "node_count mismatch: header={}, data={}",
            header.node_count,
            snapshot_data.nodes.len()
        )));
    }
    if header.edge_count != total_edges {
        return Err(PersistenceError::ValidationFailed(format!(
            "edge_count mismatch: header={}, data={}",
            header.edge_count, total_edges
        )));
    }
    if header.string_count != snapshot_data.strings.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "string_count mismatch: header={}, data={}",
            header.string_count,
            snapshot_data.strings.len()
        )));
    }
    if header.file_count != snapshot_data.files.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "file_count mismatch: header={}, data={}",
            header.file_count,
            snapshot_data.files.len()
        )));
    }

    validate_snapshot_semantics(snapshot_data)?;

    Ok(())
}

/// Validates header counts against a deserialized V9 snapshot.
#[allow(dead_code)] // Preserved as reference; V10 live paths bypass this.
fn validate_loaded_snapshot_v9(
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotDataV9,
) -> Result<(), PersistenceError> {
    let forward_stats = snapshot_data.edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;

    if header.node_count != snapshot_data.nodes.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "node_count mismatch: header={}, data={}",
            header.node_count,
            snapshot_data.nodes.len()
        )));
    }
    if header.edge_count != total_edges {
        return Err(PersistenceError::ValidationFailed(format!(
            "edge_count mismatch: header={}, data={}",
            header.edge_count, total_edges
        )));
    }
    if header.string_count != snapshot_data.strings.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "string_count mismatch: header={}, data={}",
            header.string_count,
            snapshot_data.strings.len()
        )));
    }
    if header.file_count != snapshot_data.files.len() {
        return Err(PersistenceError::ValidationFailed(format!(
            "file_count mismatch: header={}, data={}",
            header.file_count,
            snapshot_data.files.len()
        )));
    }

    validate_snapshot_semantics_v9(snapshot_data)?;

    Ok(())
}

/// Validates semantic invariants of a deserialized V8 snapshot (legacy; kept for
/// reference — all live load paths now use [`validate_snapshot_semantics_v9`]).
#[allow(dead_code)]
fn validate_snapshot_semantics(snapshot_data: &GraphSnapshotData) -> Result<(), PersistenceError> {
    for (node_id, entry) in snapshot_data.nodes.iter() {
        // Skip inert merged-losers from Phase 4c-prime cross-file node
        // unification — those arena slots are kept `Occupied` so
        // `NodeArena::slot_count()` stays stable for CSR row_ptr
        // sizing, but `merge_node_into` clears their `name` to
        // `StringId::INVALID` and their `qualified_name` to `None`.
        // They are name-invisible by construction (see
        // `AuxiliaryIndices::build_from_arena`), so the resolver
        // never surfaces them; validating resolver-eligibility fields
        // would be both wrong (they have no resolvable name) and a
        // regression on the Gate 0d iter-1 blocker fix.
        if entry.name == crate::graph::unified::string::StringId::INVALID {
            continue;
        }

        let file_path = snapshot_data.files.resolve(entry.file).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved file id {:?}; run `sqry index` to rebuild",
                entry.file
            ))
        })?;

        let _name = snapshot_data.strings.resolve(entry.name).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved name string id {:?}; run `sqry index` to rebuild",
                entry.name
            ))
        })?;

        let Some(qualified_name_id) = entry.qualified_name else {
            continue;
        };

        let qualified_name =
            snapshot_data
                .strings
                .resolve(qualified_name_id)
                .ok_or_else(|| {
                    PersistenceError::ValidationFailed(format!(
                        "resolver-eligible node {node_id:?} has unresolved qualified-name string id {qualified_name_id:?}; run `sqry index` to rebuild"
                    ))
                })?;

        let language = snapshot_data
            .files
            .language_for_file(entry.file)
            .ok_or_else(|| {
                PersistenceError::ValidationFailed(format!(
                    "resolver-eligible node {node_id:?} in '{}' is missing file language metadata; run `sqry index` to rebuild",
                    file_path.display()
                ))
            })?;

        if !is_canonical_graph_qualified_name(language, qualified_name.as_ref()) {
            return Err(PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} in '{}' stores non-canonical qualified name '{}'; run `sqry index` to rebuild",
                file_path.display(),
                qualified_name
            )));
        }
    }

    Ok(())
}

/// Validates semantic invariants of a deserialized V9 snapshot.
///
/// Checks every live node's file and name string IDs against the registry and
/// interner. V9-specific stores (scope_arena, alias_table, shadow_table,
/// scope_provenance) are not cross-validated here — they carry internal
/// consistency invariants enforced during derivation.
#[allow(dead_code)] // Preserved as reference; V10 writers bypass this.
fn validate_snapshot_semantics_v9(
    snapshot_data: &GraphSnapshotDataV9,
) -> Result<(), PersistenceError> {
    for (node_id, entry) in snapshot_data.nodes.iter() {
        // Skip inert merged-losers (see `validate_snapshot_semantics`
        // above for the full contract).
        if entry.name == crate::graph::unified::string::StringId::INVALID {
            continue;
        }

        let file_path = snapshot_data.files.resolve(entry.file).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved file id {:?}; run `sqry index` to rebuild",
                entry.file
            ))
        })?;

        let _name = snapshot_data.strings.resolve(entry.name).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved name string id {:?}; run `sqry index` to rebuild",
                entry.name
            ))
        })?;

        let Some(qualified_name_id) = entry.qualified_name else {
            continue;
        };

        let qualified_name =
            snapshot_data
                .strings
                .resolve(qualified_name_id)
                .ok_or_else(|| {
                    PersistenceError::ValidationFailed(format!(
                        "resolver-eligible node {node_id:?} has unresolved qualified-name string id {qualified_name_id:?}; run `sqry index` to rebuild"
                    ))
                })?;

        let language = snapshot_data
            .files
            .language_for_file(entry.file)
            .ok_or_else(|| {
                PersistenceError::ValidationFailed(format!(
                    "resolver-eligible node {node_id:?} in '{}' is missing file language metadata; run `sqry index` to rebuild",
                    file_path.display()
                ))
            })?;

        if !is_canonical_graph_qualified_name(language, qualified_name.as_ref()) {
            return Err(PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} in '{}' stores non-canonical qualified name '{}'; run `sqry index` to rebuild",
                file_path.display(),
                qualified_name
            )));
        }
    }

    Ok(())
}

/// Validates snapshot semantics for V11 (same rules as V10, operates on V11 fields).
///
/// V11 carries the same resolver-eligibility invariants as V10; the additive
/// `c_indirect_tables` slot has no resolver-eligibility consequence and is
/// not checked here.
#[allow(dead_code)] // Preserved as reference; V12 writers bypass this.
fn validate_snapshot_semantics_v11(
    snapshot_data: &GraphSnapshotDataV11,
) -> Result<(), PersistenceError> {
    for (node_id, entry) in snapshot_data.nodes.iter() {
        if entry.name == crate::graph::unified::string::StringId::INVALID {
            continue;
        }

        let file_path = snapshot_data.files.resolve(entry.file).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved file id {:?}; run `sqry index` to rebuild",
                entry.file
            ))
        })?;

        let _name = snapshot_data.strings.resolve(entry.name).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved name string id {:?}; run `sqry index` to rebuild",
                entry.name
            ))
        })?;

        let Some(qualified_name_id) = entry.qualified_name else {
            continue;
        };

        let qualified_name =
            snapshot_data
                .strings
                .resolve(qualified_name_id)
                .ok_or_else(|| {
                    PersistenceError::ValidationFailed(format!(
                        "resolver-eligible node {node_id:?} has unresolved qualified-name string id {qualified_name_id:?}; run `sqry index` to rebuild"
                    ))
                })?;

        let language = snapshot_data
            .files
            .language_for_file(entry.file)
            .ok_or_else(|| {
                PersistenceError::ValidationFailed(format!(
                    "resolver-eligible node {node_id:?} in '{}' is missing file language metadata; run `sqry index` to rebuild",
                    file_path.display()
                ))
            })?;

        if !is_canonical_graph_qualified_name(language, qualified_name.as_ref()) {
            return Err(PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} in '{}' stores non-canonical qualified name '{}'; run `sqry index` to rebuild",
                file_path.display(),
                qualified_name
            )));
        }
    }

    Ok(())
}

/// Validates snapshot semantics for V12 (same rules as V11, operates on V12 fields).
///
/// V12 carries the same resolver-eligibility invariants as V11; the additive
/// joint-stub slots (`framework_routes_table`, `dispatch_tables_table`) have
/// no resolver-eligibility consequence and are not checked here.
fn validate_snapshot_semantics_v12(
    snapshot_data: &GraphSnapshotDataV12,
) -> Result<(), PersistenceError> {
    for (node_id, entry) in snapshot_data.nodes.iter() {
        if entry.name == crate::graph::unified::string::StringId::INVALID {
            continue;
        }

        let file_path = snapshot_data.files.resolve(entry.file).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved file id {:?}; run `sqry index` to rebuild",
                entry.file
            ))
        })?;

        let _name = snapshot_data.strings.resolve(entry.name).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved name string id {:?}; run `sqry index` to rebuild",
                entry.name
            ))
        })?;

        let Some(qualified_name_id) = entry.qualified_name else {
            continue;
        };

        let qualified_name =
            snapshot_data
                .strings
                .resolve(qualified_name_id)
                .ok_or_else(|| {
                    PersistenceError::ValidationFailed(format!(
                        "resolver-eligible node {node_id:?} has unresolved qualified-name string id {qualified_name_id:?}; run `sqry index` to rebuild"
                    ))
                })?;

        let language = snapshot_data
            .files
            .language_for_file(entry.file)
            .ok_or_else(|| {
                PersistenceError::ValidationFailed(format!(
                    "resolver-eligible node {node_id:?} in '{}' is missing file language metadata; run `sqry index` to rebuild",
                    file_path.display()
                ))
            })?;

        if !is_canonical_graph_qualified_name(language, qualified_name.as_ref()) {
            return Err(PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} in '{}' stores non-canonical qualified name '{}'; run `sqry index` to rebuild",
                file_path.display(),
                qualified_name
            )));
        }
    }

    Ok(())
}

/// Validates snapshot semantics for V10 (same rules as V9, operates on V10 fields).
#[allow(dead_code)] // Preserved as reference; V11 writers bypass this.
fn validate_snapshot_semantics_v10(
    snapshot_data: &GraphSnapshotDataV10,
) -> Result<(), PersistenceError> {
    for (node_id, entry) in snapshot_data.nodes.iter() {
        // Skip inert merged-losers from Phase 4c-prime cross-file node
        // unification. Their slots stay `Occupied` so CSR sizing is
        // preserved, but `merge_node_into` clears `name` /
        // `qualified_name` on the loser. The resolver never surfaces
        // them (see `AuxiliaryIndices::build_from_arena`), so
        // resolver-eligibility validation must treat them as
        // intentionally-inert rather than corrupt.
        if entry.name == crate::graph::unified::string::StringId::INVALID {
            continue;
        }

        let file_path = snapshot_data.files.resolve(entry.file).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved file id {:?}; run `sqry index` to rebuild",
                entry.file
            ))
        })?;

        let _name = snapshot_data.strings.resolve(entry.name).ok_or_else(|| {
            PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} has unresolved name string id {:?}; run `sqry index` to rebuild",
                entry.name
            ))
        })?;

        let Some(qualified_name_id) = entry.qualified_name else {
            continue;
        };

        let qualified_name =
            snapshot_data
                .strings
                .resolve(qualified_name_id)
                .ok_or_else(|| {
                    PersistenceError::ValidationFailed(format!(
                        "resolver-eligible node {node_id:?} has unresolved qualified-name string id {qualified_name_id:?}; run `sqry index` to rebuild"
                    ))
                })?;

        let language = snapshot_data
            .files
            .language_for_file(entry.file)
            .ok_or_else(|| {
                PersistenceError::ValidationFailed(format!(
                    "resolver-eligible node {node_id:?} in '{}' is missing file language metadata; run `sqry index` to rebuild",
                    file_path.display()
                ))
            })?;

        if !is_canonical_graph_qualified_name(language, qualified_name.as_ref()) {
            return Err(PersistenceError::ValidationFailed(format!(
                "resolver-eligible node {node_id:?} in '{}' stores non-canonical qualified name '{}'; run `sqry index` to rebuild",
                file_path.display(),
                qualified_name
            )));
        }
    }

    Ok(())
}

/// Computes the next monotonic fact-layer epoch for a save operation.
///
/// If `snapshot_path` exists and contains a valid V7 or V8 header, reads
/// the previous `fact_epoch` and returns `max(prev + 1, now_secs)`. If the
/// file does not exist or the header cannot be read, falls back to
/// `now_secs` (which is correct for a fresh build).
///
/// This function only reads the magic bytes + fixed-length header prefix —
/// it does **not** load the data section, so the cost is negligible.
fn next_fact_epoch(snapshot_path: &Path) -> u64 {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let prev_epoch = read_prev_fact_epoch(snapshot_path).unwrap_or(0);
    std::cmp::max(prev_epoch + 1, now_secs)
}

/// Reads only the `fact_epoch` from an existing snapshot file's header.
///
/// Returns `None` on any I/O or parse error (file missing, corrupt magic,
/// truncated header, etc.). Callers treat `None` as `prev_epoch = 0`.
fn read_prev_fact_epoch(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let mut reader = BufReader::new(file);

    let (_version, header_len, _consumed) = read_magic_and_header_len(&mut reader).ok()?;
    if header_len > MAX_HEADER_BYTES {
        return None;
    }

    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf).ok()?;
    let header: GraphHeader = postcard::from_bytes(&header_buf).ok()?;
    Some(header.fact_epoch())
}

/// Converts a deserialized V7 snapshot into the V8 shape by attaching
/// empty provenance stores sized to the arena/CSR slot counts.
fn upconvert_v7_to_v8(v7: GraphSnapshotDataV7) -> GraphSnapshotData {
    let node_slot_count = v7.nodes.slot_count();
    let edge_count = {
        let stats = v7.edges.stats().forward;
        stats.csr_edge_count + stats.delta_edge_count
    };

    let mut node_provenance = NodeProvenanceStore::new();
    node_provenance.resize_to(node_slot_count);

    let mut edge_provenance = EdgeProvenanceStore::new();
    edge_provenance.resize_to(edge_count);

    GraphSnapshotData {
        nodes: v7.nodes,
        edges: v7.edges,
        strings: v7.strings,
        files: v7.files,
        indices: v7.indices,
        macro_metadata: v7.macro_metadata,
        node_provenance,
        edge_provenance,
    }
}

/// Upconverts a deserialized V8 snapshot into V9 by running Phase 4e binding-plane
/// derivation on the materialized graph.
///
/// This function:
/// 1. Materializes a `CodeGraph` from the V8 components.
/// 2. Stamps the supplied `fact_epoch` (taken from the source `GraphHeader.fact_epoch`
///    by V8 callers, or `0` for the V7→V8→V9 chained path because V7 has no
///    fact-epoch field) onto the materialized graph **before** Phase 4e runs, so
///    every scope-provenance entry minted by `derive_binding_plane` carries the
///    real source epoch instead of being silently demoted to `0`.
/// 3. Runs `derive_binding_plane(&mut graph)` to populate `scope_arena`,
///    `alias_table`, `shadow_table`, and `scope_provenance_store`.
/// 4. Returns a `GraphSnapshotDataV9` ready for validation and `CodeGraph` construction.
///
/// The reverse index on `ScopeProvenanceStore` is built during derivation;
/// no separate `rebuild_reverse_index` call is required here.
///
/// ## Why a parameter rather than a field on `GraphSnapshotData`
///
/// V8's persisted `GraphSnapshotData` shape did not carry the fact epoch — the
/// epoch lives only on the V8 `GraphHeader`. Callers that have decoded the
/// V8 header pass `header.fact_epoch()`; the V7 chained call site passes `0`
/// explicitly because V7 has no header `fact_epoch`. Threading the epoch as an
/// explicit parameter keeps the V8 on-disk schema unchanged while restoring
/// correct provenance stamping during inline upconvert.
fn upconvert_v8_to_v9(v8: GraphSnapshotData, fact_epoch: u64) -> GraphSnapshotDataV9 {
    let node_provenance = v8.node_provenance;
    let edge_provenance = v8.edge_provenance;

    let mut graph = CodeGraph::from_components(
        v8.nodes,
        v8.edges,
        v8.strings,
        v8.files,
        v8.indices,
        v8.macro_metadata,
    );
    graph.set_provenance(node_provenance, edge_provenance, fact_epoch);

    // Run Phase 4e binding-plane derivation (scopes, aliases, shadows, provenance).
    derive_binding_plane(&mut graph);

    // Extract V9 fields from the now-derived graph.
    let snapshot = graph.snapshot();
    let scope_arena = snapshot.scope_arena().clone();
    let alias_table = snapshot.alias_table().clone();
    let shadow_table = snapshot.shadow_table().clone();
    let scope_provenance = snapshot.scope_provenance_store().clone();

    // Re-extract V8 fields (the underlying stores are unchanged by Phase 4e).
    let node_prov = snapshot.nodes().iter().fold(
        {
            let mut s = NodeProvenanceStore::new();
            s.resize_to(snapshot.nodes().slot_count());
            s
        },
        |mut acc, (nid, _)| {
            if let Some(p) = snapshot.node_provenance(nid) {
                acc.insert(nid, *p);
            }
            acc
        },
    );

    use crate::graph::unified::edge::id::EdgeId;
    use crate::graph::unified::storage::edge_provenance::EdgeProvenance;
    let edge_stats = snapshot.edges().stats().forward;
    let total_edges = edge_stats.csr_edge_count + edge_stats.delta_edge_count;
    let mut edge_prov = EdgeProvenanceStore::new();
    edge_prov.resize_to(total_edges);
    for edge_idx in 0..total_edges {
        if let Ok(idx) = u32::try_from(edge_idx) {
            let eid = EdgeId::new(idx);
            if eid.is_valid() {
                let p = snapshot
                    .edge_provenance(eid)
                    .cloned()
                    .unwrap_or_else(|| EdgeProvenance::fresh(0));
                edge_prov.insert(eid, p);
            }
        }
    }

    GraphSnapshotDataV9 {
        nodes: snapshot.nodes().clone(),
        edges: snapshot.edges().clone(),
        strings: snapshot.strings().clone(),
        files: snapshot.files().clone(),
        indices: snapshot.indices().clone(),
        macro_metadata: snapshot.macro_metadata().clone(),
        node_provenance: node_prov,
        edge_provenance: edge_prov,
        scope_arena,
        alias_table,
        shadow_table,
        scope_provenance,
    }
}

/// Upconvert a V9 snapshot to V10 by rebuilding the `FileSegmentTable` from
/// the node arena and translating live edge / metadata stores into V10
/// wire-format mirror types.
///
/// The segment table maps each `FileId` to its contiguous `(start_slot, slot_count)`
/// range in the arena. Since V9 does not persist this table, we scan the arena
/// to derive it: for each file, find the min and max occupied slot indices, then
/// record `(min, max - min + 1)`.
///
/// V9 fields `edges` / `macro_metadata` are currently live types, so we
/// translate them through the V11 → V10 helpers before stuffing them into
/// `GraphSnapshotDataV10` (which holds the V10 wire-format mirrors after
/// U03). This keeps the V7 → V8 → V9 → V10 → V11 chain typeable end-to-end.
fn upconvert_v9_to_v10(v9: GraphSnapshotDataV9) -> GraphSnapshotDataV10 {
    use super::legacy_v10::{
        translate_bidirectional_edge_store_v11_to_v10, translate_metadata_store_v11_to_v10,
    };

    let file_segments = rebuild_file_segments_from_arena(&v9.nodes);

    // Translate live types into V10 wire-format mirrors. The V11 → V10
    // direction is a structural mirror; the only failure mode for
    // `translate_metadata_store_v11_to_v10` is a metadata entry whose
    // `(typed, flags)` combination V10 cannot represent — which by
    // construction cannot happen on a V9 input (V9 predates `NodeFlags`
    // entirely, so every entry deserialized through V8 / V9 already
    // satisfies the V10 invariant).
    let edges = translate_bidirectional_edge_store_v11_to_v10(&v9.edges);
    let macro_metadata = translate_metadata_store_v11_to_v10(&v9.macro_metadata)
        .expect("V9 snapshot carries pre-U02 metadata shapes that always fit V10");

    GraphSnapshotDataV10 {
        nodes: v9.nodes,
        edges,
        strings: v9.strings,
        files: v9.files,
        indices: v9.indices,
        macro_metadata,
        node_provenance: v9.node_provenance,
        edge_provenance: v9.edge_provenance,
        scope_arena: v9.scope_arena,
        alias_table: v9.alias_table,
        shadow_table: v9.shadow_table,
        scope_provenance: v9.scope_provenance,
        file_segments,
    }
}

/// Upconvert a V10 snapshot to V11 by translating the V10 wire-format mirror
/// types ([`super::legacy_v10::BidirectionalEdgeStoreV10`],
/// [`super::legacy_v10::NodeMetadataStoreV10`]) into the live (V11) shapes
/// and stamping the reserved `c_indirect_tables: None` envelope slot.
///
/// # Per-element reshape (codex iter-1 BLOCKER fix)
///
/// Prior to U03 codex iter-1, this function received a `GraphSnapshotDataV10`
/// whose `edges` / `macro_metadata` were already typed as the **live**
/// `BidirectionalEdgeStore` / `NodeMetadataStore`. That arrangement assumed
/// the V10 postcard bytes happened to share the live serde shape — which is
/// only true while:
///
/// * `EdgeKind::Calls` carries exactly the V10 fields (`argument_count`,
///   `is_async`). U04 adds `resolved_via`, breaking the assumption.
/// * `NodeMetadataStore::Deserialize` is the pre-U02 three-variant decoder.
///   U02 ships a new entry layout with a `flags` byte, breaking it.
///
/// To insulate the V10 wire format from those live-type changes, U03 pins
/// `GraphSnapshotDataV10.edges` to [`super::legacy_v10::BidirectionalEdgeStoreV10`]
/// and `GraphSnapshotDataV10.macro_metadata` to
/// [`super::legacy_v10::NodeMetadataStoreV10`]. This function then performs
/// the explicit translation:
///
/// * `legacy_v10::EdgeKindV10::Calls { argument_count, is_async }` →
///   `EdgeKind::Calls { argument_count, is_async }`. When U04 lands the
///   `resolved_via: ResolvedVia` field on `EdgeKind::Calls`, the live `Calls`
///   arm in [`super::legacy_v10::translate_edge_v10_to_v11`] will fail to
///   compile (missing field) — that compile error is the deliberate
///   integration anchor. The TODO comment there documents the one-line fix
///   (stamp `resolved_via: ResolvedVia::Direct`; V10 snapshots predate the
///   indirect-call resolver so every V10 `Calls` edge is, by construction,
///   directly resolved).
/// * `NodeMetadataV10::Macro(m)`     → `StoredEntry { typed: Some(TypedMetadata::Macro(m)), flags: NodeFlags::EMPTY }`
/// * `NodeMetadataV10::Classpath(c)` → `StoredEntry { typed: Some(TypedMetadata::Classpath(c)), flags: NodeFlags::EMPTY }`
/// * `NodeMetadataV10::Synthetic`    → `StoredEntry { typed: None, flags: NodeFlags::SYNTHETIC }`
///
/// All other fields carry over by value.
///
/// # Errors
///
/// Returns `PersistenceError::Serialization` if a V10 metadata entry
/// declares its typed kind but carries no matching payload (corrupt
/// snapshot).
fn upconvert_v10_to_v11(
    v10: GraphSnapshotDataV10,
) -> Result<GraphSnapshotDataV11, PersistenceError> {
    use super::legacy_v10::{
        translate_bidirectional_edge_store_v10_to_v11, translate_metadata_store_v10_to_v11,
    };

    let edges = translate_bidirectional_edge_store_v10_to_v11(v10.edges);
    let macro_metadata = translate_metadata_store_v10_to_v11(v10.macro_metadata)?;

    Ok(GraphSnapshotDataV11 {
        nodes: v10.nodes,
        edges,
        strings: v10.strings,
        files: v10.files,
        indices: v10.indices,
        macro_metadata,
        node_provenance: v10.node_provenance,
        edge_provenance: v10.edge_provenance,
        scope_arena: v10.scope_arena,
        alias_table: v10.alias_table,
        shadow_table: v10.shadow_table,
        scope_provenance: v10.scope_provenance,
        file_segments: v10.file_segments,
        c_indirect_tables: None,
    })
}

/// Extract the Phase β joint-stub side tables from a metadata store into the
/// V12 envelope wire shapes.
///
/// The in-memory `NodeMetadataStore` stores `framework_routes` as a
/// `BTreeMap<NodeId, FrameworkRouteMetadata>`; the V12 envelope serializes
/// the same data as a flat `Vec<(NodeId, FrameworkRouteMetadata)>` to keep
/// postcard's typed-key constraints satisfied. `dispatch_tables` round-trips
/// verbatim. Both are cloned out of the snapshot — no mutation of the
/// source store.
fn extract_phase_beta_side_tables(
    macro_metadata: &NodeMetadataStore,
) -> (
    Vec<(
        crate::graph::unified::node::id::NodeId,
        crate::graph::unified::storage::FrameworkRouteMetadata,
    )>,
    DispatchTables,
) {
    let framework_routes_table: Vec<_> = macro_metadata
        .framework_routes()
        .iter()
        .map(|(&node_id, meta)| (node_id, meta.clone()))
        .collect();
    let dispatch_tables_table = macro_metadata.dispatch_tables().clone();
    (framework_routes_table, dispatch_tables_table)
}

/// Reattach the Phase β joint-stub envelope slots onto the in-memory
/// metadata store after V12 deserialization.
///
/// The metadata-store custom serde impl doesn't carry the new fields (V11
/// metadata-store wire compatibility is preserved bit-for-bit). The V12
/// envelope carries them in dedicated slots; this helper rebuilds the
/// `BTreeMap` and reattaches both via the store's setter API.
fn attach_phase_beta_side_tables(
    macro_metadata: &mut NodeMetadataStore,
    framework_routes_table: Vec<(
        crate::graph::unified::node::id::NodeId,
        crate::graph::unified::storage::FrameworkRouteMetadata,
    )>,
    dispatch_tables_table: DispatchTables,
) {
    let mut framework_routes = FrameworkRoutesMap::default();
    for (node_id, meta) in framework_routes_table {
        framework_routes.insert(node_id, meta);
    }
    macro_metadata.set_framework_routes(framework_routes);
    macro_metadata.set_dispatch_tables(dispatch_tables_table);
}

/// Upconvert a V11 [`GraphSnapshotDataV11`] to V12 by zero-initialising the
/// two new joint-stub envelope slots (`framework_routes_table` +
/// `dispatch_tables_table`).
///
/// # Phase β joint-stubs
///
/// Plan A's framework-route extractors and Plan B's dispatch-resolver
/// passes both target V12. The single joint-stub PR adds both envelope
/// slots so a workspace upgrading from V11 sees a strictly additive
/// schema bump: every V11 field carries over by value, and the two new
/// slots come up empty. No metadata-store reshape is needed (the V11
/// metadata-store wire format is preserved bit-for-bit inside the V12
/// envelope).
///
/// # Errors
///
/// Infallible — all V11 fields move by value into the V12 envelope and
/// the new slots are zero-initialised. The `Result` return is kept for
/// symmetry with the other upconvert functions in this module, which
/// may surface translation errors when wire shapes differ across versions.
fn upconvert_v11_to_v12(
    v11: GraphSnapshotDataV11,
) -> Result<GraphSnapshotDataV12, PersistenceError> {
    Ok(GraphSnapshotDataV12 {
        nodes: v11.nodes,
        edges: v11.edges,
        strings: v11.strings,
        files: v11.files,
        indices: v11.indices,
        macro_metadata: v11.macro_metadata,
        node_provenance: v11.node_provenance,
        edge_provenance: v11.edge_provenance,
        scope_arena: v11.scope_arena,
        alias_table: v11.alias_table,
        shadow_table: v11.shadow_table,
        scope_provenance: v11.scope_provenance,
        file_segments: v11.file_segments,
        c_indirect_tables: v11.c_indirect_tables,
        framework_routes_table: Vec::new(),
        dispatch_tables_table: DispatchTables::default(),
    })
}

/// Upconvert a V12 snapshot to V13 (T3 — error chains: `EdgeKind::Wraps`).
///
/// V13 is shape-identical to V12 on the wire (see [`GraphSnapshotDataV13`]),
/// so this is an identity mapping. The function exists as a named upconvert
/// step so the V12 read path in `load_from_bytes` / `load_from_path` follows
/// the same chained-ladder pattern as the V7→V8→V9→V10→V11→V12 chain, and so
/// any future T3-related migration logic has a documented attachment point. A
/// V12 snapshot predates the `EdgeKind::Wraps` variant, so re-labelling it as
/// V13 is sound — there are no Wraps edges to translate.
///
/// # Errors
///
/// Infallible — all V12 fields move by value into the V13 envelope. The
/// `Result` return is kept for symmetry with the other upconvert functions in
/// this module, which may surface translation errors when wire shapes differ
/// across versions.
#[inline]
fn upconvert_v12_to_v13(
    v12: GraphSnapshotDataV12,
) -> Result<GraphSnapshotDataV13, PersistenceError> {
    Ok(v12)
}

/// Rebuilds a `FileSegmentTable` by scanning the node arena.
///
/// For each occupied slot, records the `FileId` and slot index. Then for each
/// file, computes `(min_slot, max_slot - min_slot + 1)`.
///
/// This is used during V9→V10 upconversion and as a fallback when loading
/// snapshots without a persisted segment table.
pub fn rebuild_file_segments_from_arena(arena: &NodeArena) -> FileSegmentTable {
    use crate::graph::unified::file::id::FileId;
    use std::collections::HashMap;

    let mut file_ranges: HashMap<FileId, (u32, u32)> = HashMap::new(); // (min, max)

    for (idx, slot) in arena.slots().iter().enumerate() {
        if let Some(entry) = slot.get() {
            let fid = entry.file;
            if fid != FileId::INVALID {
                let slot_idx = idx as u32;
                file_ranges
                    .entry(fid)
                    .and_modify(|(min, max)| {
                        if slot_idx < *min {
                            *min = slot_idx;
                        }
                        if slot_idx > *max {
                            *max = slot_idx;
                        }
                    })
                    .or_insert((slot_idx, slot_idx));
            }
        }
    }

    let mut table = FileSegmentTable::with_capacity(file_ranges.len());
    for (fid, (min, max)) in file_ranges {
        table.record_range(fid, min, max - min + 1);
    }
    table
}

/// Read a little-endian u32 from a reader.
/// Reads magic bytes and header length from a reader, handling both 13-byte
/// (V7-V9) and 14-byte (V10) magic sequences.
///
/// Returns `(FormatVersion, header_length, bytes_consumed)`.
fn read_magic_and_header_len(
    reader: &mut impl Read,
) -> Result<(FormatVersion, usize, u64), PersistenceError> {
    // Read 14 bytes to cover the longest magic (V10/V11/V12 = 14 bytes).
    // If the file is shorter, `read_exact` returns an IO error, which is fine —
    // a valid snapshot is always longer than 14 bytes.
    let mut magic = [0u8; 14];
    reader.read_exact(&mut magic)?;

    let format_version =
        FormatVersion::from_magic(&magic).ok_or_else(|| PersistenceError::InvalidMagic {
            expected: MAGIC_BYTES_V13.to_vec(),
            found: magic.to_vec(),
        })?;

    // V10–V13 magics are all 14 bytes — all consumed. Read full u32 header length.
    // V7-V9 magic is 13 bytes — byte 14 is the first byte of the header length.
    if matches!(
        format_version,
        FormatVersion::V10 | FormatVersion::V11 | FormatVersion::V12 | FormatVersion::V13
    ) {
        let hl = read_u32_le(reader)? as usize;
        Ok((format_version, hl, 18)) // 14 magic + 4 header_len
    } else {
        let mut rest = [0u8; 3];
        reader.read_exact(&mut rest)?;
        let hl = u32::from_le_bytes([magic[13], rest[0], rest[1], rest[2]]) as usize;
        Ok((format_version, hl, 17)) // 14 read + 3 rest = 17; equivalent to 13 magic + 4 header_len
    }
}

fn read_u32_le(reader: &mut impl Read) -> Result<u32, std::io::Error> {
    let mut buf = [0u8; 4];
    reader.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// Read a little-endian u64 from a reader.
fn read_u64_le(reader: &mut impl Read) -> Result<u64, std::io::Error> {
    let mut buf = [0u8; 8];
    reader.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// Saves a graph to the specified path.
///
/// The graph is serialized using postcard with length-prefixed framing:
/// 1. Magic bytes (`SQRY_GRAPH_V5`)
/// 2. Header length (LE u32)
/// 3. Header (postcard-encoded `GraphHeader`)
/// 4. Data length (LE u64)
/// 5. Snapshot data (postcard-encoded `GraphSnapshotData`)
///
/// Builds populated provenance stores by iterating a graph snapshot (P1U08).
///
/// Every live node gets `first_seen_epoch == last_seen_epoch == epoch` with a
/// content hash derived from its `body_hash` field (zero-padded from 16 to 32
/// bytes; the SHA-256-shaped field is reserved for when the build pipeline
/// stamps it directly from file bytes in a future unit). Every live edge gets
/// `first_seen_epoch == last_seen_epoch == epoch`.
///
/// Warm-start preservation (carrying `first_seen_epoch` from a prior snapshot)
/// is deferred to when `CodeGraph` holds the stores and the build pipeline can
/// call `NodeProvenance::touch()` on surviving nodes during rebuild.
/// Builds fresh provenance stores from scratch for a snapshot that has no
/// prior provenance (first save or V7 upconvert).
fn build_provenance_from_snapshot(
    snapshot: &crate::graph::unified::concurrent::GraphSnapshot,
    epoch: u64,
) -> (NodeProvenanceStore, EdgeProvenanceStore) {
    use crate::graph::unified::edge::id::EdgeId;
    use crate::graph::unified::storage::edge_provenance::EdgeProvenance;
    use crate::graph::unified::storage::node_provenance::NodeProvenance;

    // Node provenance: iterate live arena nodes
    let nodes = snapshot.nodes();
    let mut node_prov = NodeProvenanceStore::new();
    node_prov.resize_to(nodes.slot_count());
    for (node_id, entry) in nodes.iter() {
        let content_hash = node_content_hash(entry);
        node_prov.insert(node_id, NodeProvenance::fresh(epoch, content_hash));
    }

    // Edge provenance: stamp every edge slot in the forward store
    let edge_stats = snapshot.edges().stats().forward;
    let total_edges = edge_stats.csr_edge_count + edge_stats.delta_edge_count;
    let mut edge_prov = EdgeProvenanceStore::new();
    edge_prov.resize_to(total_edges);
    for edge_idx in 0..total_edges {
        if let Ok(idx) = u32::try_from(edge_idx) {
            let eid = EdgeId::new(idx);
            if eid.is_valid() {
                edge_prov.insert(eid, EdgeProvenance::fresh(epoch));
            }
        }
    }

    (node_prov, edge_prov)
}

/// Extracts a 32-byte content hash from a node entry's `body_hash`.
fn node_content_hash(entry: &crate::graph::unified::storage::NodeEntry) -> [u8; 32] {
    match entry.body_hash {
        Some(bh) => {
            let mut hash = [0u8; 32];
            let bh_bytes = bh.as_u128().to_le_bytes();
            hash[..16].copy_from_slice(&bh_bytes);
            hash
        }
        None => [0u8; 32],
    }
}

/// Merges existing provenance with the current snapshot state instead of
/// rebuilding from scratch.
///
/// For each live node/edge:
/// - If prior provenance exists: preserves `first_seen_epoch`, advances
///   `last_seen_epoch` to `epoch`, and refreshes `content_hash` from the
///   current node body.
/// - If no prior provenance: seeds a fresh record with `first_seen == epoch`.
///
/// For node provenance, preservation respects generational indices: the prior
/// record is carried forward only if the stored generation matches the current
/// `NodeId::generation()`. Stale-generation slots receive fresh provenance.
///
/// For edge provenance, preservation is limited to load/save round-trips where
/// slot identity is unchanged: edges carry no generation field, so slot reuse
/// across rebuilds cannot be detected. This is safe because save/load
/// round-trips preserve CSR slot layout.
fn merge_provenance_from_snapshot(
    snapshot: &crate::graph::unified::concurrent::GraphSnapshot,
    epoch: u64,
) -> (NodeProvenanceStore, EdgeProvenanceStore) {
    use crate::graph::unified::edge::id::EdgeId;
    use crate::graph::unified::storage::edge_provenance::EdgeProvenance;
    use crate::graph::unified::storage::node_provenance::NodeProvenance;

    // --- Node provenance ---
    let nodes = snapshot.nodes();
    let mut node_prov = NodeProvenanceStore::new();
    node_prov.resize_to(nodes.slot_count());

    for (node_id, entry) in nodes.iter() {
        let content_hash = node_content_hash(entry);
        let provenance = match snapshot.node_provenance(node_id) {
            Some(existing) => {
                // Preserve first_seen, advance last_seen, refresh hash.
                NodeProvenance {
                    first_seen_epoch: existing.first_seen_epoch,
                    last_seen_epoch: epoch,
                    content_hash,
                }
            }
            None => NodeProvenance::fresh(epoch, content_hash),
        };
        node_prov.insert(node_id, provenance);
    }

    // --- Edge provenance ---
    let edge_stats = snapshot.edges().stats().forward;
    let total_edges = edge_stats.csr_edge_count + edge_stats.delta_edge_count;
    let mut edge_prov = EdgeProvenanceStore::new();
    edge_prov.resize_to(total_edges);

    for edge_idx in 0..total_edges {
        if let Ok(idx) = u32::try_from(edge_idx) {
            let eid = EdgeId::new(idx);
            if eid.is_valid() {
                let provenance = match snapshot.edge_provenance(eid) {
                    Some(existing) => {
                        // Preserve first_seen, advance last_seen.
                        EdgeProvenance {
                            first_seen_epoch: existing.first_seen_epoch,
                            last_seen_epoch: epoch,
                        }
                    }
                    None => EdgeProvenance::fresh(epoch),
                };
                edge_prov.insert(eid, provenance);
            }
        }
    }

    (node_prov, edge_prov)
}

/// Returns merged provenance stores when the snapshot already carries
/// provenance (loaded from a prior save), or fresh stores when it does not.
fn resolve_provenance(
    snapshot: &crate::graph::unified::concurrent::GraphSnapshot,
    epoch: u64,
) -> (NodeProvenanceStore, EdgeProvenanceStore) {
    if snapshot.fact_epoch() > 0 {
        // The graph was loaded from a persisted snapshot and carries prior
        // provenance. Merge rather than rebuild to preserve first_seen_epoch.
        merge_provenance_from_snapshot(snapshot, epoch)
    } else {
        // First save or V7 upconvert — no prior provenance exists.
        build_provenance_from_snapshot(snapshot, epoch)
    }
}

/// Stamps `indexed_at` on every registered file entry in the registry.
///
/// Called at save time so that every `FileEntry` in a single build carries the
/// same `indexed_at` value (equal to the snapshot's `fact_epoch`). Content hash
/// and source URI remain at their current values (defaulted to zero / None for
/// fresh builds until the build pipeline stamps them from file bytes).
fn stamp_file_indexed_at(files: &mut FileRegistry, epoch: u64) {
    use crate::graph::unified::file::id::FileId;

    // FileRegistry entries are 1-indexed (slot 0 is the INVALID sentinel).
    // Use slot_count() to cover every allocated slot, including vacant/recycled
    // ones whose indices exceed len(). The setter is a no-op for
    // invalid/vacant slots.
    let slot_count = files.slot_count();
    for idx in 1..slot_count {
        if let Ok(i) = u32::try_from(idx) {
            let fid = FileId::new(i);
            files.set_indexed_at(fid, epoch);
        }
    }
}

/// Writes a framed V9 snapshot to a buffered writer.
///
/// Frame layout (same as V8 but with `MAGIC_BYTES_V9`):
/// 1. Magic bytes (`SQRY_GRAPH_V9`, 13 bytes)
/// 2. Header length (LE u32)
/// 3. Header (postcard-encoded `GraphHeader`)
/// 4. Data length (LE u64)
/// 5. Data (postcard-encoded `GraphSnapshotDataV9`)
#[allow(clippy::cast_possible_truncation)] // data_bytes.len() validated < max_snapshot_bytes()
#[allow(dead_code)] // Preserved for V9 compatibility reference.
fn write_framed_v9(
    writer: &mut BufWriter<File>,
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotDataV9,
) -> Result<(), PersistenceError> {
    // Invariant: interner lookup must be consistent before serialization.
    debug_assert!(
        !snapshot_data.strings.is_lookup_stale(),
        "Cannot serialize StringInterner with stale lookup — \
         call build_dedup_table() before saving"
    );

    let header_bytes = postcard::to_allocvec(header)?;
    let data_bytes = postcard::to_allocvec(snapshot_data)?;

    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(format!(
            "header too large to save: {} bytes exceeds MAX_HEADER_BYTES ({} bytes)",
            header_bytes.len(),
            MAX_HEADER_BYTES,
        )));
    }
    let max_data_bytes = max_snapshot_bytes();
    if data_bytes.len() as u64 > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large to save: {} bytes exceeds limit ({} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES if the codebase legitimately requires a larger snapshot",
            data_bytes.len(),
            max_data_bytes,
        )));
    }

    writer.write_all(MAGIC_BYTES_V9)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Writes a V10 snapshot with length-prefixed framing.
///
/// Generic over any [`Write`] implementor so the function can be
/// driven by either a `BufWriter<File>` (the legacy direct-write
/// path used by tests) or a `BufWriter<&mut NamedTempFile>` (the
/// atomic-write path introduced by `G_daemon_control_plane.md`
/// §4.2).
#[allow(dead_code)] // Preserved for reference; live writers emit V13.
fn write_framed_v10<W: Write>(
    writer: &mut W,
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotDataV10,
) -> Result<(), PersistenceError> {
    debug_assert!(
        !snapshot_data.strings.is_lookup_stale(),
        "Cannot serialize StringInterner with stale lookup — \
         call build_dedup_table() before saving"
    );

    let header_bytes = postcard::to_allocvec(header)?;
    let data_bytes = postcard::to_allocvec(snapshot_data)?;

    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(format!(
            "header too large to save: {} bytes exceeds MAX_HEADER_BYTES ({} bytes)",
            header_bytes.len(),
            MAX_HEADER_BYTES,
        )));
    }
    let max_data_bytes = max_snapshot_bytes();
    if data_bytes.len() as u64 > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large to save: {} bytes exceeds limit ({} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES if the codebase legitimately requires a larger snapshot",
            data_bytes.len(),
            max_data_bytes,
        )));
    }

    writer.write_all(MAGIC_BYTES_V10)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Writes a V11 snapshot with length-prefixed framing.
///
/// V11 was the writer format before the Phase β joint-stubs bump to V12.
/// Retained as a reference path and exercised by the legacy-V11 fixture
/// tests; the live writer is [`write_framed_v12`].
#[allow(dead_code)] // Preserved as reference; live writers emit V12.
fn write_framed_v11<W: Write>(
    writer: &mut W,
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotDataV11,
) -> Result<(), PersistenceError> {
    debug_assert!(
        !snapshot_data.strings.is_lookup_stale(),
        "Cannot serialize StringInterner with stale lookup — \
         call build_dedup_table() before saving"
    );

    let header_bytes = postcard::to_allocvec(header)?;
    let data_bytes = postcard::to_allocvec(snapshot_data)?;

    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(format!(
            "header too large to save: {} bytes exceeds MAX_HEADER_BYTES ({} bytes)",
            header_bytes.len(),
            MAX_HEADER_BYTES,
        )));
    }
    let max_data_bytes = max_snapshot_bytes();
    if data_bytes.len() as u64 > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large to save: {} bytes exceeds limit ({} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES if the codebase legitimately requires a larger snapshot",
            data_bytes.len(),
            max_data_bytes,
        )));
    }

    writer.write_all(MAGIC_BYTES_V11)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Writes a V12 snapshot with length-prefixed framing.
///
/// V12 was the writer format for the Phase β joint-stubs schema. It extends
/// V11 with two new envelope slots — `framework_routes_table` (Plan A) and
/// `dispatch_tables_table` (Plan B). It was the live writer before the T3
/// error-chains bump to V13; retained as a reference path and exercised by
/// the legacy-V12 fixture tests. The live writer is [`write_framed_v13`].
#[allow(dead_code)] // Preserved as reference; live writers emit V13.
fn write_framed_v12<W: Write>(
    writer: &mut W,
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotDataV12,
) -> Result<(), PersistenceError> {
    debug_assert!(
        !snapshot_data.strings.is_lookup_stale(),
        "Cannot serialize StringInterner with stale lookup — \
         call build_dedup_table() before saving"
    );

    let header_bytes = postcard::to_allocvec(header)?;
    let data_bytes = postcard::to_allocvec(snapshot_data)?;

    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(format!(
            "header too large to save: {} bytes exceeds MAX_HEADER_BYTES ({} bytes)",
            header_bytes.len(),
            MAX_HEADER_BYTES,
        )));
    }
    let max_data_bytes = max_snapshot_bytes();
    if data_bytes.len() as u64 > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large to save: {} bytes exceeds limit ({} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES if the codebase legitimately requires a larger snapshot",
            data_bytes.len(),
            max_data_bytes,
        )));
    }

    writer.write_all(MAGIC_BYTES_V12)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Writes a V13 snapshot with length-prefixed framing.
///
/// V13 is the current writer format (T3 — error chains: `EdgeKind::Wraps`).
/// It is shape-identical to V12 on the wire (see [`GraphSnapshotDataV13`]);
/// only the magic-byte prefix differs, so that V12 readers reject snapshots
/// whose `EdgeKind` arena contains the new `Wraps` variant rather than
/// silently mis-decoding an unknown discriminant. The frame shape (magic +
/// header + data) is identical to V12.
fn write_framed_v13<W: Write>(
    writer: &mut W,
    header: &GraphHeader,
    snapshot_data: &GraphSnapshotDataV13,
) -> Result<(), PersistenceError> {
    debug_assert!(
        !snapshot_data.strings.is_lookup_stale(),
        "Cannot serialize StringInterner with stale lookup — \
         call build_dedup_table() before saving"
    );

    let header_bytes = postcard::to_allocvec(header)?;
    let data_bytes = postcard::to_allocvec(snapshot_data)?;

    if header_bytes.len() > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(format!(
            "header too large to save: {} bytes exceeds MAX_HEADER_BYTES ({} bytes)",
            header_bytes.len(),
            MAX_HEADER_BYTES,
        )));
    }
    let max_data_bytes = max_snapshot_bytes();
    if data_bytes.len() as u64 > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large to save: {} bytes exceeds limit ({} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES if the codebase legitimately requires a larger snapshot",
            data_bytes.len(),
            max_data_bytes,
        )));
    }

    writer.write_all(MAGIC_BYTES_V13)?;
    writer.write_all(
        &u32::try_from(header_bytes.len())
            .map_err(|_| {
                PersistenceError::ValidationFailed(
                    "header too large for u32 length prefix".to_string(),
                )
            })?
            .to_le_bytes(),
    )?;
    writer.write_all(&header_bytes)?;
    writer.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
    writer.write_all(&data_bytes)?;

    writer.flush()?;
    Ok(())
}

/// Atomic snapshot writer per `G_daemon_control_plane.md` §4.2 +
/// `00_contracts.md` §3.CC-4 hand-off G5.
///
/// Steps (per the design):
///
/// 1. Create a [`NamedTempFile`] in the **same parent directory** as
///    the destination path so the subsequent `persist`-rename stays
///    on the same filesystem (cross-FS rename is non-atomic).
/// 2. Hand the writer to the caller's `write_fn` callback.
/// 3. `fsync` the data file so a power-loss between rename and
///    persist cannot leave a zero-length / partial snapshot.
/// 4. `persist` (atomic rename) into the destination path.
/// 5. `fsync` the parent directory so the rename itself survives
///    a power-loss.
/// 6. Stale-temp cleanup is automatic: [`NamedTempFile::drop`]
///    unlinks the temp file on every early-return path, so a
///    callback that errors out leaves no stale file behind.
///
/// If the parent directory does not exist, returns an `io::Error`.
/// The caller is expected to ensure the parent exists (sqry's
/// `GraphStorage::ensure_dir` does this at index-creation time).
///
/// # Errors
///
/// Returns [`PersistenceError`] when the temp file cannot be
/// created, the callback errors, the fsync fails, or the rename
/// fails. The destination path is never partially-overwritten:
/// either the new bytes are atomically renamed into place, or
/// the prior snapshot is left intact.
fn write_atomic<W>(path: &Path, write_fn: W) -> Result<(), PersistenceError>
where
    W: FnOnce(&mut BufWriter<&mut NamedTempFile>) -> Result<(), PersistenceError>,
{
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temp = NamedTempFile::new_in(parent).map_err(PersistenceError::Io)?;

    {
        let mut writer = BufWriter::new(&mut temp);
        write_fn(&mut writer)?;
        writer.flush().map_err(PersistenceError::Io)?;
    }

    // Step 3 — fsync the data file. The file handle is the inner
    // `&File` of the NamedTempFile.
    temp.as_file().sync_all().map_err(PersistenceError::Io)?;

    // Step 4 — atomic rename. `persist_noclobber` would race with
    // a concurrent writer; we want last-writer-wins semantics for
    // the snapshot, matching the legacy `File::create` behaviour.
    temp.persist(path)
        .map_err(|e| PersistenceError::Io(e.error))?;

    // Step 5 — fsync the parent directory so the rename survives
    // a power loss. On Windows, opening directories is not
    // supported by `File::open`; the rename is still atomic but
    // crash-recovery semantics are weaker. The cfg gate keeps
    // the build clean.
    //
    // Cluster-G iter-2: surface parent-fsync errors instead of
    // silently swallowing them. The codex iter-1 review flagged
    // that returning `Ok(())` after an fsync failure overstates
    // the crash-safety contract on ext4 / btrfs / xfs. Either the
    // open fails (no fsync was possible) or the sync fails
    // (durability not guaranteed); both are reported as `Io`.
    #[cfg(unix)]
    {
        let dir = File::open(parent).map_err(PersistenceError::Io)?;
        dir.sync_all().map_err(PersistenceError::Io)?;
    }

    Ok(())
}

/// # Errors
///
/// Returns an error if the file cannot be created or serialization fails.
pub fn save_to_path(graph: &CodeGraph, path: impl AsRef<Path>) -> Result<(), PersistenceError> {
    let path = path.as_ref();

    // Stamp a monotonic fact epoch BEFORE creating the temp file
    // (which inherits the destination's place in the fact-epoch
    // sequence). The temp file is created in the same parent dir
    // by `write_atomic` so the subsequent rename stays on-FS and
    // is atomic.
    let fact_epoch = next_fact_epoch(path);

    // Get a snapshot of the graph
    let snapshot = graph.snapshot();

    // Preserve existing provenance when the graph already carries it (from a
    // prior load), or build fresh provenance for first-time saves.
    let (node_provenance, edge_provenance) = resolve_provenance(&snapshot, fact_epoch);

    // Extract components from snapshot
    let nodes = snapshot.nodes().clone();
    let edges = snapshot.edges().clone();
    let strings = snapshot.strings().clone();
    let mut files = snapshot.files().clone();
    let indices = snapshot.indices().clone();
    let macro_metadata = snapshot.macro_metadata().clone();

    // Stamp file-level provenance: indexed_at = fact_epoch for every entry
    stamp_file_indexed_at(&mut files, fact_epoch);

    // Extract Phase 2 binding-plane stores from the snapshot.
    let scope_arena = snapshot.scope_arena().clone();
    let alias_table = snapshot.alias_table().clone();
    let shadow_table = snapshot.shadow_table().clone();
    let scope_provenance = snapshot.scope_provenance_store().clone();

    // Extract Phase 3 file segment table.
    let file_segments = snapshot.file_segments().clone();

    // Phase A (U09): persist the C indirect-call side tables. `None` on
    // non-C workspaces — the snapshot accessor returns `Option<&...>` which
    // we clone into the envelope. The persistence wire-shape contract
    // (DESIGN §10.2) is `Option<CIndirectSideTables>` with `#[serde(default)]`
    // gating; legacy V10 snapshots round-trip as `None`.
    let c_indirect_tables = snapshot.c_indirect_tables().cloned();

    // Phase β joint-stubs (V12): persist the Plan A / Plan B side tables.
    // Both are empty on V11 → V12 upconverts and on every workspace where
    // no framework extractor / dispatch resolver has populated them yet.
    let (framework_routes_table, dispatch_tables_table) =
        extract_phase_beta_side_tables(snapshot.macro_metadata());

    let snapshot_data = GraphSnapshotDataV12 {
        nodes,
        edges,
        strings,
        files,
        indices,
        macro_metadata,
        node_provenance,
        edge_provenance,
        scope_arena,
        alias_table,
        shadow_table,
        scope_provenance,
        file_segments,
        c_indirect_tables,
        framework_routes_table,
        dispatch_tables_table,
    };

    validate_snapshot_semantics_v12(&snapshot_data)?;

    // Create header — V13 with stamped fact_epoch and correct version tag.
    // V13 is shape-identical to V12, so the V12 validator accepts it.
    let forward_stats = snapshot_data.edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
    let mut header = GraphHeader::new(
        snapshot_data.nodes.len(),
        total_edges,
        snapshot_data.strings.len(),
        snapshot_data.files.len(),
    );
    header.version = FormatVersion::V13.as_u32();
    header.set_fact_epoch(fact_epoch);

    // Atomic write per `G_daemon_control_plane.md` §4.2: an
    // interrupted write leaves the prior snapshot intact rather
    // than producing a zero-length / corrupted file (which `index
    // --status` previously misreported as "No graph snapshot
    // found").
    write_atomic(path, |writer| {
        write_framed_v13(writer, &header, &snapshot_data)
    })
}

/// Saves a graph to the specified path with config provenance.
///
/// This is the recommended save method when building graphs, as it records
/// the configuration used to build the graph for reproducibility tracking.
///
/// # Errors
///
/// Returns an error if the file cannot be created or serialization fails.
pub fn save_to_path_with_provenance(
    graph: &CodeGraph,
    path: impl AsRef<Path>,
    provenance: ConfigProvenance,
    plugins: &PluginManager,
) -> Result<(), PersistenceError> {
    let path = path.as_ref();

    // Stamp a monotonic fact epoch BEFORE the temp file.
    let fact_epoch = next_fact_epoch(path);

    // Get a snapshot of the graph
    let snapshot = graph.snapshot();

    // Preserve existing provenance when the graph already carries it (from a
    // prior load), or build fresh provenance for first-time saves.
    let (node_provenance, edge_provenance) = resolve_provenance(&snapshot, fact_epoch);

    // Extract components from snapshot
    let nodes = snapshot.nodes().clone();
    let edges = snapshot.edges().clone();
    let strings = snapshot.strings().clone();
    let mut files = snapshot.files().clone();
    let indices = snapshot.indices().clone();
    let macro_metadata = snapshot.macro_metadata().clone();

    // Stamp file-level provenance: indexed_at = fact_epoch for every entry
    stamp_file_indexed_at(&mut files, fact_epoch);

    // Collect plugin versions
    let plugin_versions: HashMap<String, String> = plugins
        .plugins()
        .iter()
        .map(|p| {
            let meta = p.metadata();
            (meta.id.to_string(), meta.version.to_string())
        })
        .collect();

    // Extract Phase 2 binding-plane stores from the snapshot.
    let scope_arena = snapshot.scope_arena().clone();
    let alias_table = snapshot.alias_table().clone();
    let shadow_table = snapshot.shadow_table().clone();
    let scope_provenance = snapshot.scope_provenance_store().clone();

    // Extract Phase 3 file segment table.
    let file_segments = snapshot.file_segments().clone();

    // Phase A (U09): persist the C indirect-call side tables. See the
    // sibling save path's comment for the wire-shape contract.
    let c_indirect_tables = snapshot.c_indirect_tables().cloned();

    // Phase β joint-stubs (V12): see the sibling save path's comment.
    let (framework_routes_table, dispatch_tables_table) =
        extract_phase_beta_side_tables(snapshot.macro_metadata());

    let snapshot_data = GraphSnapshotDataV12 {
        nodes,
        edges,
        strings,
        files,
        indices,
        macro_metadata,
        node_provenance,
        edge_provenance,
        scope_arena,
        alias_table,
        shadow_table,
        scope_provenance,
        file_segments,
        c_indirect_tables,
        framework_routes_table,
        dispatch_tables_table,
    };

    validate_snapshot_semantics_v12(&snapshot_data)?;

    // Create header with provenance, plugin versions, V13 version tag, and fact epoch.
    // V13 is shape-identical to V12, so the V12 validator accepts it.
    let forward_stats = snapshot_data.edges.stats().forward;
    let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
    let mut header = GraphHeader::with_provenance_and_plugins(
        snapshot_data.nodes.len(),
        total_edges,
        snapshot_data.strings.len(),
        snapshot_data.files.len(),
        provenance,
        plugin_versions,
    );
    header.version = FormatVersion::V13.as_u32();
    header.set_fact_epoch(fact_epoch);

    // Atomic write per `G_daemon_control_plane.md` §4.2.
    write_atomic(path, |writer| {
        write_framed_v13(writer, &header, &snapshot_data)
    })
}

/// Validates that plugin versions in the graph match current plugin versions.
fn validate_plugin_versions(
    header: &GraphHeader,
    plugins: &PluginManager,
) -> Result<(), PersistenceError> {
    // Collect current plugin versions
    let current_versions: HashMap<String, String> = plugins
        .plugins()
        .iter()
        .map(|p| {
            let meta = p.metadata();
            (meta.id.to_string(), meta.version.to_string())
        })
        .collect();

    // Check each plugin that was used to build the index
    for (plugin_id, stored_version) in header.plugin_versions() {
        match current_versions.get(plugin_id) {
            Some(current_version) if current_version != stored_version => {
                return Err(PersistenceError::PluginVersionMismatch {
                    plugin_id: plugin_id.clone(),
                    expected: current_version.clone(),
                    found: stored_version.clone(),
                });
            }
            None => {
                // Plugin was used to build index but is no longer available
                return Err(PersistenceError::PluginVersionMismatch {
                    plugin_id: plugin_id.clone(),
                    expected: "not installed".to_string(),
                    found: stored_version.clone(),
                });
            }
            Some(_) => {
                // Version matches, continue
            }
        }
    }

    Ok(())
}

/// Verify snapshot bytes against the expected SHA256 hash from the manifest.
///
/// If `expected_sha256` is empty (pre-hash index format), verification is
/// skipped and `Ok(())` is returned.
///
/// # Errors
///
/// Returns an error if the computed SHA256 does not match `expected_sha256`.
pub fn verify_snapshot_bytes(data: &[u8], expected_sha256: &str) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};

    if expected_sha256.is_empty() {
        return Ok(());
    }

    let actual_hash = hex::encode(Sha256::digest(data));
    if actual_hash != expected_sha256 {
        anyhow::bail!(
            "Snapshot integrity check failed: expected SHA256 {expected_sha256}, got {actual_hash}. \
             The index may be corrupt or tampered with. Run `sqry index` to rebuild.",
        );
    }
    Ok(())
}

/// Loads a graph from in-memory bytes.
///
/// Same validation as [`load_from_path`] but operates on a byte slice,
/// enabling single-read integrity verification (hash once, deserialize
/// from the same bytes — no TOCTOU window).
///
/// # Errors
///
/// Returns an error if the bytes are invalid, corrupt, or incompatible.
#[allow(clippy::cast_possible_truncation)] // data_len validated < max_snapshot_bytes()
pub fn load_from_bytes(
    bytes: &[u8],
    plugins: Option<&PluginManager>,
) -> Result<CodeGraph, PersistenceError> {
    let total_len = bytes.len() as u64;
    let mut reader = Cursor::new(bytes);
    let mut bytes_consumed: u64 = 0;

    let (format_version, header_len, magic_bytes) = read_magic_and_header_len(&mut reader)?;
    bytes_consumed += magic_bytes;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = total_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    bytes_consumed += header_len as u64;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version — accept V7 (legacy), V8, V9, V10, V11, V12 (upconvert), V13 (current).
    if header.version != VERSION
        && header.version != FormatVersion::V8.as_u32()
        && header.version != FormatVersion::V9.as_u32()
        && header.version != FormatVersion::V10.as_u32()
        && header.version != FormatVersion::V11.as_u32()
        && header.version != FormatVersion::V12.as_u32()
        && header.version != FormatVersion::V13.as_u32()
    {
        return Err(PersistenceError::IncompatibleVersion {
            expected: FormatVersion::V13.as_u32(),
            found: header.version,
        });
    }

    // Validate plugin versions (requires rebuild if mismatch) - skip if no plugin manager
    if let Some(plugin_manager) = plugins {
        validate_plugin_versions(&header, plugin_manager)?;
    }

    // Validate header counts before attempting data deserialization
    validate_header_sanity(&header)?;

    // Read data length and validate before allocation
    let data_len = read_u64_le(&mut reader)?;
    bytes_consumed += 8;
    let max_data_bytes = max_snapshot_bytes();
    if data_len > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large: {data_len} bytes exceeds limit ({max_data_bytes} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES to load this snapshot",
        )));
    }
    let remaining = total_len.saturating_sub(bytes_consumed);
    if data_len > remaining {
        return Err(PersistenceError::ValidationFailed(
            "data length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize data, dispatching on detected format version. Each
    // older format chains through the upconverters: V7 → V8 → V9 → V10 → V11 →
    // V12 → V13; ...; V12 → V13; V13 → direct.
    let mut data_buf = vec![0u8; data_len as usize];
    reader.read_exact(&mut data_buf)?;
    let mut snapshot_data: GraphSnapshotDataV13 = match format_version {
        FormatVersion::V7 => {
            // V7 predates `GraphHeader.fact_epoch`; pass `0` explicitly so the
            // V7 → V8 → V9 → V10 → V11 → V12 chained path is deterministic
            // and matches the documented "V7 has no fact epoch" contract.
            let v7: GraphSnapshotDataV7 = postcard::from_bytes(&data_buf)?;
            let v8 = upconvert_v7_to_v8(v7);
            let v9 = upconvert_v8_to_v9(v8, 0);
            let v10 = upconvert_v9_to_v10(v9);
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V8 => {
            // Forward the V8 header epoch so derived scope provenance is
            // stamped with the real source epoch, not silently demoted to 0.
            let v8: GraphSnapshotData = postcard::from_bytes(&data_buf)?;
            let v9 = upconvert_v8_to_v9(v8, header.fact_epoch());
            let v10 = upconvert_v9_to_v10(v9);
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V9 => {
            let v9: GraphSnapshotDataV9 = postcard::from_bytes(&data_buf)?;
            let v10 = upconvert_v9_to_v10(v9);
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V10 => {
            let v10: GraphSnapshotDataV10 = postcard::from_bytes(&data_buf)?;
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V11 => {
            let v11: GraphSnapshotDataV11 = postcard::from_bytes(&data_buf)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V12 => {
            let v12: GraphSnapshotDataV12 = postcard::from_bytes(&data_buf)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V13 => postcard::from_bytes(&data_buf)?,
    };

    // Rebuild the scope-provenance reverse index after deserialization.
    // The map is not persisted on disk; it is always derived from occupied slots.
    snapshot_data
        .scope_provenance
        .rebuild_reverse_index(&snapshot_data.scope_arena);

    // Reject trailing bytes
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? > 0 {
        return Err(PersistenceError::ValidationFailed(
            "unexpected trailing bytes after data section".to_string(),
        ));
    }

    validate_snapshot_semantics_v12(&snapshot_data)?;

    // Phase β joint-stubs: route the V12 envelope slots onto the in-memory
    // metadata store. V11 → V12 upconverts always carry empty defaults;
    // native V12/V13 snapshots may carry populated joint-stub tables once
    // Plan A's extractors / Plan B's resolvers land. (V13 is shape-identical
    // to V12, so the same envelope slots apply.)
    attach_phase_beta_side_tables(
        &mut snapshot_data.macro_metadata,
        std::mem::take(&mut snapshot_data.framework_routes_table),
        std::mem::take(&mut snapshot_data.dispatch_tables_table),
    );

    let mut graph = CodeGraph::from_components(
        snapshot_data.nodes,
        snapshot_data.edges,
        snapshot_data.strings,
        snapshot_data.files,
        snapshot_data.indices,
        snapshot_data.macro_metadata,
    );
    graph.set_provenance(
        snapshot_data.node_provenance,
        snapshot_data.edge_provenance,
        header.fact_epoch(),
    );
    graph.set_scope_arena(snapshot_data.scope_arena);
    graph.set_alias_table(snapshot_data.alias_table);
    graph.set_shadow_table(snapshot_data.shadow_table);
    graph.set_scope_provenance_store(snapshot_data.scope_provenance);
    graph.set_file_segments(snapshot_data.file_segments);
    // Phase A (U09): route the V12 `Option<CIndirectSideTables>` envelope
    // slot onto the live `CodeGraph`. V10/V11 → V12 upconverts carry the
    // V11 value unchanged; older upconverts carry `None`.
    graph.set_c_indirect_tables(snapshot_data.c_indirect_tables);
    Ok(graph)
}

/// Loads a graph from the specified path.
///
/// Reads the framed format (V7, V8, or V9) with length-prefixed sections and
/// pre-allocation validation. Rejects formats older than V7.
///
/// V7 snapshots are upconverted to V8 shape and then to V9 via
/// `derive_binding_plane`. V8 snapshots are upconverted to V9 inline.
///
/// # Errors
///
/// Returns an error if the file is invalid, corrupt, or incompatible.
#[allow(clippy::cast_possible_truncation)] // data_len validated < max_snapshot_bytes()
pub fn load_from_path(
    path: impl AsRef<Path>,
    plugins: Option<&PluginManager>,
) -> Result<CodeGraph, PersistenceError> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut bytes_consumed: u64 = 0;

    let (format_version, header_len, magic_bytes) = read_magic_and_header_len(&mut reader)?;
    bytes_consumed += magic_bytes;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    bytes_consumed += header_len as u64;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version — accept V7 (legacy), V8, V9, V10, V11, V12 (upconvert), V13 (current).
    if header.version != VERSION
        && header.version != FormatVersion::V8.as_u32()
        && header.version != FormatVersion::V9.as_u32()
        && header.version != FormatVersion::V10.as_u32()
        && header.version != FormatVersion::V11.as_u32()
        && header.version != FormatVersion::V12.as_u32()
        && header.version != FormatVersion::V13.as_u32()
    {
        return Err(PersistenceError::IncompatibleVersion {
            expected: FormatVersion::V13.as_u32(),
            found: header.version,
        });
    }

    // Validate plugin versions (requires rebuild if mismatch) - skip if no plugin manager
    if let Some(plugin_manager) = plugins {
        validate_plugin_versions(&header, plugin_manager)?;
    }

    // Validate header counts before attempting data deserialization
    validate_header_sanity(&header)?;

    // Read data length and validate before allocation
    let data_len = read_u64_le(&mut reader)?;
    bytes_consumed += 8;
    let max_data_bytes = max_snapshot_bytes();
    if data_len > max_data_bytes {
        return Err(PersistenceError::ValidationFailed(format!(
            "data section too large: {data_len} bytes exceeds limit ({max_data_bytes} bytes); \
             increase SQRY_MAX_SNAPSHOT_BYTES to load this snapshot",
        )));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if data_len > remaining {
        return Err(PersistenceError::ValidationFailed(
            "data length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize data — dispatch on format version. Each older
    // format chains through the upconverters: V7 → V8 → V9 → V10 → V11 → V12 →
    // V13; V8 → … → V13; ...; V11 → V12 → V13; V12 → V13; V13 → direct.
    let mut data_buf = vec![0u8; data_len as usize];
    reader.read_exact(&mut data_buf)?;
    let mut snapshot_data: GraphSnapshotDataV13 = match format_version {
        FormatVersion::V7 => {
            // V7 predates `GraphHeader.fact_epoch`; pass `0` explicitly so the
            // chained path is deterministic and matches the documented "V7
            // has no fact epoch" contract.
            let v7: GraphSnapshotDataV7 = postcard::from_bytes(&data_buf)?;
            let v8 = upconvert_v7_to_v8(v7);
            let v9 = upconvert_v8_to_v9(v8, 0);
            let v10 = upconvert_v9_to_v10(v9);
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V8 => {
            // Forward the V8 header epoch so derived scope provenance is
            // stamped with the real source epoch, not silently demoted to 0.
            let v8: GraphSnapshotData = postcard::from_bytes(&data_buf)?;
            let v9 = upconvert_v8_to_v9(v8, header.fact_epoch());
            let v10 = upconvert_v9_to_v10(v9);
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V9 => {
            let v9: GraphSnapshotDataV9 = postcard::from_bytes(&data_buf)?;
            let v10 = upconvert_v9_to_v10(v9);
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V10 => {
            let v10: GraphSnapshotDataV10 = postcard::from_bytes(&data_buf)?;
            let v11 = upconvert_v10_to_v11(v10)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V11 => {
            let v11: GraphSnapshotDataV11 = postcard::from_bytes(&data_buf)?;
            let v12 = upconvert_v11_to_v12(v11)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V12 => {
            let v12: GraphSnapshotDataV12 = postcard::from_bytes(&data_buf)?;
            upconvert_v12_to_v13(v12)?
        }
        FormatVersion::V13 => postcard::from_bytes(&data_buf)?,
    };

    // Rebuild the scope-provenance reverse index after deserialization.
    // The map is not persisted on disk; it is always derived from occupied slots.
    snapshot_data
        .scope_provenance
        .rebuild_reverse_index(&snapshot_data.scope_arena);

    // Reject trailing bytes
    let mut trailing = [0u8; 1];
    if reader.read(&mut trailing)? > 0 {
        return Err(PersistenceError::ValidationFailed(
            "unexpected trailing bytes after data section".to_string(),
        ));
    }

    validate_snapshot_semantics_v12(&snapshot_data)?;

    // Phase β joint-stubs: reattach the V12 envelope slots onto the
    // in-memory metadata store. V11 → V12 upconverts carry empty defaults.
    // (V13 is shape-identical to V12, so the same envelope slots apply.)
    attach_phase_beta_side_tables(
        &mut snapshot_data.macro_metadata,
        std::mem::take(&mut snapshot_data.framework_routes_table),
        std::mem::take(&mut snapshot_data.dispatch_tables_table),
    );

    let mut graph = CodeGraph::from_components(
        snapshot_data.nodes,
        snapshot_data.edges,
        snapshot_data.strings,
        snapshot_data.files,
        snapshot_data.indices,
        snapshot_data.macro_metadata,
    );
    graph.set_provenance(
        snapshot_data.node_provenance,
        snapshot_data.edge_provenance,
        header.fact_epoch(),
    );
    graph.set_scope_arena(snapshot_data.scope_arena);
    graph.set_alias_table(snapshot_data.alias_table);
    graph.set_shadow_table(snapshot_data.shadow_table);
    graph.set_scope_provenance_store(snapshot_data.scope_provenance);
    graph.set_file_segments(snapshot_data.file_segments);
    // Phase A (U09): route the V12 `Option<CIndirectSideTables>` envelope
    // slot onto the live `CodeGraph`. Older upconverts carry forward the
    // value from V11 (or `None` for V10 and older).
    graph.set_c_indirect_tables(snapshot_data.c_indirect_tables);
    Ok(graph)
}

/// Validates a graph snapshot file without fully loading it.
///
/// Checks magic bytes, version, and header deserialization.
///
/// # Errors
///
/// Returns an error if validation fails.
pub fn validate_snapshot(path: impl AsRef<Path>) -> Result<bool, PersistenceError> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut bytes_consumed: u64 = 0;

    let (_format_version, header_len, magic_bytes) = read_magic_and_header_len(&mut reader)?;
    bytes_consumed += magic_bytes;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version — accept V7 (legacy), V8, V9, V10, V11, V12 (upconvert), V13 (current).
    if header.version != VERSION
        && header.version != FormatVersion::V8.as_u32()
        && header.version != FormatVersion::V9.as_u32()
        && header.version != FormatVersion::V10.as_u32()
        && header.version != FormatVersion::V11.as_u32()
        && header.version != FormatVersion::V12.as_u32()
        && header.version != FormatVersion::V13.as_u32()
    {
        return Err(PersistenceError::IncompatibleVersion {
            expected: FormatVersion::V13.as_u32(),
            found: header.version,
        });
    }

    // Basic validation passed
    Ok(true)
}

/// Loads just the header from a graph file (fast, doesn't load graph data).
///
/// # Errors
///
/// Returns an error if the file cannot be read or is invalid.
pub fn load_header_from_path(path: impl AsRef<Path>) -> Result<GraphHeader, PersistenceError> {
    let path = path.as_ref();
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut bytes_consumed: u64 = 0;

    let (_format_version, header_len, magic_bytes) = read_magic_and_header_len(&mut reader)?;
    bytes_consumed += magic_bytes;
    if header_len > MAX_HEADER_BYTES {
        return Err(PersistenceError::ValidationFailed(
            "header too large".to_string(),
        ));
    }
    let remaining = file_len.saturating_sub(bytes_consumed);
    if (header_len as u64) > remaining {
        return Err(PersistenceError::ValidationFailed(
            "header length exceeds remaining file bytes".to_string(),
        ));
    }

    // Read and deserialize header
    let mut header_buf = vec![0u8; header_len];
    reader.read_exact(&mut header_buf)?;
    let header: GraphHeader = postcard::from_bytes(&header_buf)?;

    // Validate version — accept V7 (legacy), V8, V9, V10, V11, V12 (upconvert), V13 (current).
    if header.version != VERSION
        && header.version != FormatVersion::V8.as_u32()
        && header.version != FormatVersion::V9.as_u32()
        && header.version != FormatVersion::V10.as_u32()
        && header.version != FormatVersion::V11.as_u32()
        && header.version != FormatVersion::V12.as_u32()
        && header.version != FormatVersion::V13.as_u32()
    {
        return Err(PersistenceError::IncompatibleVersion {
            expected: FormatVersion::V13.as_u32(),
            found: header.version,
        });
    }

    Ok(header)
}

/// Checks if a graph's config has drifted from the current config.
///
/// # Errors
///
/// Returns an error if the graph header cannot be read or if provenance is missing.
pub fn check_config_drift(
    graph_path: impl AsRef<Path>,
    current_checksum: &str,
) -> Result<bool, PersistenceError> {
    let header = load_header_from_path(graph_path)?;

    match header.config_provenance {
        Some(provenance) => Ok(provenance.config_matches(current_checksum)),
        None => Err(PersistenceError::ValidationFailed(
            "Graph has no config provenance".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::super::format::{MAGIC_BYTES, MAGIC_BYTES_V8};
    use super::super::manifest::{OverrideEntry, OverrideSource};
    use super::*;
    use crate::graph::node::Language;
    use crate::graph::unified::file::FileId;
    use crate::graph::unified::node::NodeKind;
    use crate::graph::unified::storage::NodeEntry;
    use tempfile::NamedTempFile;

    // Test helper to create an empty plugin manager
    fn create_test_plugin_manager() -> PluginManager {
        PluginManager::new()
    }

    fn write_snapshot_fixture(
        path: &Path,
        snapshot_data: &GraphSnapshotData,
    ) -> Result<(), PersistenceError> {
        let forward_stats = snapshot_data.edges.stats().forward;
        let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
        let header = GraphHeader::new(
            snapshot_data.nodes.len(),
            total_edges,
            snapshot_data.strings.len(),
            snapshot_data.files.len(),
        );
        let header_bytes = postcard::to_allocvec(&header)?;
        let data_bytes = postcard::to_allocvec(snapshot_data)?;

        let mut file = File::create(path)?;
        // Phase 1: write V8 magic so round-trip tests pass through the V8 reader.
        file.write_all(MAGIC_BYTES_V8)?;
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )?;
        file.write_all(&header_bytes)?;
        file.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
        file.write_all(&data_bytes)?;
        file.flush()?;
        Ok(())
    }

    fn graph_with_one_node(
        qualified_name: &str,
        language: Language,
        file_path: &Path,
    ) -> CodeGraph {
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(file_path, Some(language))
            .unwrap();
        let name_id = graph.strings_mut().intern("target").unwrap();
        let qname_id = graph.strings_mut().intern(qualified_name).unwrap();
        let entry = NodeEntry::new(NodeKind::Function, name_id, file_id)
            .with_location(1, 0, 1, 6)
            .with_qualified_name(qname_id);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();
        graph.indices_mut().add(
            node_id,
            entry.kind,
            entry.name,
            entry.qualified_name,
            entry.file,
        );
        graph
    }

    /// End-to-end V13 persistence proof for the T3 `EdgeKind::Wraps`
    /// variant: a graph carrying a single `Wraps` edge with a non-default
    /// `kind` + `chain_position` must survive a V13 save and reload through
    /// BOTH `load_from_path` and `load_from_bytes` with both fields intact.
    ///
    /// This is the round-trip coverage the V13 renumber needs: the snapshot
    /// magic bump exists precisely so a snapshot whose edge arena contains
    /// `Wraps` is stamped V13 (rejected by V12 readers), and the on-disk
    /// `EdgeKind` discriminant for `Wraps` must deserialize losslessly.
    #[test]
    fn v13_wraps_edge_survives_round_trip() {
        use crate::graph::unified::edge::{EdgeKind, WrapKind};

        // Two-node graph + one Wraps edge. Non-default kind/position keep the
        // assertion non-vacuous (a dropped or zeroed field would fail).
        let mut graph = graph_with_one_node(
            "errpkg::Wrap",
            Language::Go,
            Path::new("/v13_wraps_test.go"),
        );
        let file_id = graph.files().get(Path::new("/v13_wraps_test.go")).unwrap();
        let name2 = graph.strings_mut().intern("Cause").unwrap();
        let qname2 = graph.strings_mut().intern("errpkg::Cause").unwrap();
        let entry2 = NodeEntry::new(NodeKind::Function, name2, file_id)
            .with_location(5, 0, 5, 10)
            .with_qualified_name(qname2);
        let node2 = graph.nodes_mut().alloc(entry2).unwrap();
        let node1 = graph.nodes().iter().next().unwrap().0;
        let _ = graph.edges().add_edge(
            node1,
            node2,
            EdgeKind::Wraps {
                kind: WrapKind::ErrorsAs,
                chain_position: Some(3),
            },
            file_id,
        );

        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        save_to_path(&graph, path).expect("save_to_path");

        // On-disk magic must be V13 (a Wraps-bearing graph cannot be V12).
        let raw = std::fs::read(path).expect("read snapshot bytes");
        assert_eq!(
            &raw[..MAGIC_BYTES_V13.len()],
            MAGIC_BYTES_V13.as_slice(),
            "Wraps-bearing graph must persist as V13",
        );

        fn assert_wraps_survived(graph: &CodeGraph) {
            let snap = graph.snapshot();
            let wraps: Vec<_> = snap
                .edges()
                .all_live_forward_edges()
                .into_iter()
                .filter(|e| matches!(e.kind, EdgeKind::Wraps { .. }))
                .collect();
            assert_eq!(wraps.len(), 1, "exactly one Wraps edge must survive");
            match &wraps[0].kind {
                EdgeKind::Wraps {
                    kind,
                    chain_position,
                } => {
                    assert_eq!(*kind, WrapKind::ErrorsAs, "WrapKind must round-trip");
                    assert_eq!(*chain_position, Some(3), "chain_position must round-trip");
                }
                other => panic!("expected EdgeKind::Wraps, got {other:?}"),
            }
        }

        // Reload through both loader entry points (V13 arm in each).
        let loaded_path = load_from_path(path, None).expect("load_from_path V13");
        assert_wraps_survived(&loaded_path);

        let loaded_bytes = load_from_bytes(&raw, None).expect("load_from_bytes V13");
        assert_wraps_survived(&loaded_bytes);
    }

    #[test]
    fn test_save_load_empty_graph() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Save
        save_to_path(&graph, path).unwrap();

        // Validate
        assert!(validate_snapshot(path).unwrap());

        // Load
        let loaded = load_from_path(path, Some(&plugins)).unwrap();
        let snapshot = loaded.snapshot();

        assert_eq!(snapshot.nodes().len(), 0);
        assert_eq!(snapshot.strings().len(), 0);
        assert_eq!(snapshot.files().len(), 0);
    }

    #[test]
    fn test_save_load_with_provenance() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Create provenance
        let provenance = ConfigProvenance::new(
            ".sqry/graph/config/config.json",
            "abc123checksum".to_string(),
            1,
        );

        // Save with provenance
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Load header and check provenance
        let header = load_header_from_path(path).unwrap();
        assert!(header.has_provenance());

        let loaded_provenance = header.provenance().unwrap();
        assert_eq!(loaded_provenance.config_checksum, "abc123checksum");
        assert_eq!(loaded_provenance.schema_version, 1);
    }

    #[test]
    fn test_config_drift_detection() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Create provenance with known checksum
        let provenance = ConfigProvenance::new(
            ".sqry/graph/config/config.json",
            "original_checksum".to_string(),
            1,
        );

        // Save with provenance
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Check drift - same checksum should match
        assert!(check_config_drift(path, "original_checksum").unwrap());

        // Check drift - different checksum should not match
        assert!(!check_config_drift(path, "different_checksum").unwrap());
    }

    #[test]
    fn test_config_drift_no_provenance() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Save without provenance
        save_to_path(&graph, path).unwrap();

        // Check drift should fail - no provenance
        let result = check_config_drift(path, "any_checksum");
        assert!(result.is_err());
    }

    #[test]
    fn test_provenance_with_overrides() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Create provenance with overrides
        let mut provenance =
            ConfigProvenance::new(".sqry/graph/config/config.json", "checksum".to_string(), 1);
        provenance.add_override(OverrideEntry {
            source: OverrideSource::Cli,
            key: "parallelism.max_workers".to_string(),
            value: "16".to_string(),
            original_value: Some("8".to_string()),
        });

        // Save
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Load and verify overrides
        let header = load_header_from_path(path).unwrap();
        let loaded_provenance = header.provenance().unwrap();

        assert!(loaded_provenance.has_overrides());
        assert_eq!(loaded_provenance.override_count(), 1);
    }

    #[test]
    fn test_load_rejects_invalid_magic() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write garbage magic bytes
        let mut file = File::create(path).unwrap();
        file.write_all(b"NOT_SQRY_MAGIC").unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::InvalidMagic { .. } => {}
            other => panic!("Expected InvalidMagic, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_v3_snapshot() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write V3 magic bytes (old format) + padding to 14 bytes so
        // `read_magic_and_header_len` gets past `read_exact` and returns
        // `InvalidMagic` (not `Io::UnexpectedEof`).
        let mut file = File::create(path).unwrap();
        file.write_all(b"SQRY_GRAPH_V3\x00").unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::InvalidMagic { .. } => {}
            other => panic!("Expected InvalidMagic for V3 snapshot, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_corrupted_header_counts() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write a valid V4 file with corrupted header counts
        let corrupt_header = GraphHeader::new(
            100_000_001, // Corrupted node_count (just over the limit)
            0,
            0,
            0,
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )
        .unwrap();
        file.write_all(&header_bytes).unwrap();
        // Write dummy data length and no data
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());

        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable node_count"));
                assert!(msg.contains("corrupted"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_header_length_exceeding_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write magic + header_len that exceeds remaining file bytes
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&999_999u32.to_le_bytes()).unwrap(); // header_len way too big
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("header length exceeds remaining file bytes"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_data_length_exceeding_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Write valid magic + valid header + data_len exceeding file
        let header = GraphHeader::new(0, 0, 0, 0);
        let header_bytes = postcard::to_allocvec(&header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )
        .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&999_999u64.to_le_bytes()).unwrap(); // data_len way too big
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("data length exceeds remaining file bytes"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_trailing_bytes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Save a valid graph
        let graph = CodeGraph::new();
        save_to_path(&graph, path).unwrap();

        // Append trailing bytes
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"junk").unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("trailing bytes"));
            }
            other => panic!("Expected ValidationFailed for trailing bytes, got: {other:?}"),
        }
    }

    #[test]
    fn test_save_rejects_non_canonical_qualified_name() {
        let graph = graph_with_one_node(
            "pkg.module.target",
            Language::Python,
            Path::new("/tmp/test.py"),
        );
        let temp_file = NamedTempFile::new().unwrap();

        let result = save_to_path(&graph, temp_file.path());
        assert!(result.is_err());

        match result.unwrap_err() {
            PersistenceError::ValidationFailed(message) => {
                assert!(message.contains("non-canonical qualified name"));
                assert!(message.contains("sqry index"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_non_canonical_qualified_name() {
        let graph = graph_with_one_node(
            "pkg::module::target",
            Language::Python,
            Path::new("/tmp/test.py"),
        );
        let snapshot = graph.snapshot();
        let mut snapshot_data = GraphSnapshotData {
            nodes: snapshot.nodes().clone(),
            edges: snapshot.edges().clone(),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: snapshot.macro_metadata().clone(),
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
        };
        let temp_file = NamedTempFile::new().unwrap();
        let plugins = create_test_plugin_manager();

        let invalid_qname_id = snapshot_data.strings.intern("pkg.module.target").unwrap();
        let (node_id, entry) = snapshot_data.nodes.iter().next().unwrap();
        let entry_kind = entry.kind;
        let entry_name = entry.name;
        let entry_file = entry.file;
        snapshot_data.nodes.get_mut(node_id).unwrap().qualified_name = Some(invalid_qname_id);
        snapshot_data.indices.clear();
        snapshot_data.indices.add(
            node_id,
            entry_kind,
            entry_name,
            Some(invalid_qname_id),
            entry_file,
        );

        write_snapshot_fixture(temp_file.path(), &snapshot_data).unwrap();

        let result = load_from_path(temp_file.path(), Some(&plugins));
        assert!(result.is_err());

        match result.unwrap_err() {
            PersistenceError::ValidationFailed(message) => {
                assert!(message.contains("non-canonical qualified name"));
                assert!(message.contains("sqry index"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_node_with_unresolved_file_id() {
        let mut graph = CodeGraph::new();
        let registered_file = graph
            .files_mut()
            .register_with_language(Path::new("/tmp/test.rs"), Some(Language::Rust))
            .unwrap();
        let name_id = graph.strings_mut().intern("target").unwrap();
        let qname_id = graph.strings_mut().intern("pkg::target").unwrap();
        let invalid_file_id = FileId::new(registered_file.index() + 100);
        let entry = NodeEntry::new(NodeKind::Function, name_id, invalid_file_id)
            .with_location(1, 0, 1, 6)
            .with_qualified_name(qname_id);
        let node_id = graph.nodes_mut().alloc(entry.clone()).unwrap();
        graph.indices_mut().add(
            node_id,
            entry.kind,
            entry.name,
            entry.qualified_name,
            entry.file,
        );

        let snapshot = graph.snapshot();
        let snapshot_data = GraphSnapshotData {
            nodes: snapshot.nodes().clone(),
            edges: snapshot.edges().clone(),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: snapshot.macro_metadata().clone(),
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
        };
        let temp_file = NamedTempFile::new().unwrap();
        let plugins = create_test_plugin_manager();

        write_snapshot_fixture(temp_file.path(), &snapshot_data).unwrap();

        let result = load_from_path(temp_file.path(), Some(&plugins));
        assert!(result.is_err());

        match result.unwrap_err() {
            PersistenceError::ValidationFailed(message) => {
                assert!(message.contains("unresolved file id"));
                assert!(message.contains("sqry index"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_large_edge_count() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let corrupt_header = GraphHeader::new(
            100,
            1_000_001_000, // Edge count exceeds limit
            10,
            1,
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )
        .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable edge_count"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_large_string_count() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let corrupt_header = GraphHeader::new(
            100, 1000, 50_001_000, // String count exceeds limit
            1,
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )
        .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable string_count"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_large_file_count() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let corrupt_header = GraphHeader::new(
            100, 1000, 1000, 1_001_000, // File count exceeds limit
        );
        let header_bytes = postcard::to_allocvec(&corrupt_header).unwrap();

        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )
        .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&0u64.to_le_bytes()).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(msg.contains("Unreasonable file_count"));
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_plugin_version_tracking() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        let provenance = ConfigProvenance::new(
            ".sqry/graph/config/config.json",
            "test_checksum".to_string(),
            1,
        );

        // Save with plugin versions
        save_to_path_with_provenance(&graph, path, provenance, &plugins).unwrap();

        // Load header and verify plugin versions are empty (no plugins registered)
        let header = load_header_from_path(path).unwrap();
        assert_eq!(header.plugin_versions().len(), 0);

        // Load should succeed with matching plugin manager
        let loaded = load_from_path(path, Some(&plugins)).unwrap();
        assert_eq!(loaded.snapshot().nodes().len(), 0);
    }

    #[test]
    fn test_load_rejects_header_exceeding_max_header_bytes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        #[allow(clippy::cast_possible_truncation)]
        // Snapshot data lengths validated < max_snapshot_bytes()
        // Write magic, then a header_len that exceeds MAX_HEADER_BYTES (1 MB)
        let declared_header_len: u32 = (MAX_HEADER_BYTES as u32) + 1;
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(&declared_header_len.to_le_bytes()).unwrap();
        // Write enough padding so the file is large enough that the
        // "exceeds remaining bytes" check doesn't trigger first
        let padding = vec![0u8; declared_header_len as usize + 16];
        file.write_all(&padding).unwrap();
        file.flush().unwrap();

        let result = load_from_path(path, None);
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(
                    msg.contains("header too large"),
                    "Expected 'header too large', got: {msg}"
                );
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    #[test]
    fn test_load_rejects_data_exceeding_max_snapshot_bytes() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // Build a valid header so we get past header validation
        let header = GraphHeader::new(0, 0, 0, 0);
        let header_bytes = postcard::to_allocvec(&header).unwrap();

        // Write the framed format with a data_len exceeding the active
        // snapshot byte limit. Adding 1 is safe because the runtime clamp
        // guarantees `max_snapshot_bytes() <= MAX_MAX_SNAPSHOT_BYTES < u64::MAX`.
        let declared_data_len: u64 = max_snapshot_bytes() + 1;
        let mut file = File::create(path).unwrap();
        file.write_all(MAGIC_BYTES).unwrap();
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )
        .unwrap();
        file.write_all(&header_bytes).unwrap();
        file.write_all(&declared_data_len.to_le_bytes()).unwrap();
        // We don't need actual data — the check happens before reading the data section
        file.flush().unwrap();

        let result = load_from_path(path, Some(&plugins));
        assert!(result.is_err());
        match result.unwrap_err() {
            PersistenceError::ValidationFailed(msg) => {
                assert!(
                    msg.contains("data section too large"),
                    "Expected 'data section too large', got: {msg}"
                );
            }
            other => panic!("Expected ValidationFailed, got: {other:?}"),
        }
    }

    /// Regression guard for the v8.0.0 Linux-kernel indexing failure:
    /// `sqry index` on the Linux kernel raised "data section too large to
    /// save" because the default limit had been reduced to 2 GB in the
    /// bincode → postcard migration. This test pins the active default to
    /// at least 8 GB (the pre-regression bincode-era value) so any future
    /// reduction below that threshold fails loudly.
    #[test]
    #[serial_test::serial]
    fn test_default_max_snapshot_bytes_supports_linux_kernel() {
        unsafe {
            std::env::remove_var("SQRY_MAX_SNAPSHOT_BYTES");
        }
        assert!(
            max_snapshot_bytes() >= 8 * 1024 * 1024 * 1024,
            "default snapshot limit must be >= 8 GB to support Linux-kernel-class repos; \
             got {} bytes",
            max_snapshot_bytes()
        );
    }

    #[test]
    fn test_verify_snapshot_bytes_correct_hash() {
        use sha2::{Digest, Sha256};
        let data = b"some graph snapshot data";
        let correct_hash = hex::encode(Sha256::digest(data));
        assert!(verify_snapshot_bytes(data, &correct_hash).is_ok());
    }

    #[test]
    fn test_verify_snapshot_bytes_wrong_hash() {
        let data = b"some graph snapshot data";
        let err = verify_snapshot_bytes(data, "deadbeef").unwrap_err();
        assert!(err.to_string().contains("integrity check failed"));
    }

    #[test]
    fn test_verify_snapshot_bytes_empty_hash_skips() {
        let data = b"anything";
        assert!(verify_snapshot_bytes(data, "").is_ok());
    }

    #[test]
    fn test_load_from_bytes_matches_load_from_path() {
        // Save a graph, then verify load_from_bytes produces the same result
        let plugins = crate::plugin::PluginManager::new();
        let graph = CodeGraph::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.sqry");

        save_to_path(&graph, &path).unwrap();

        let path_graph = load_from_path(&path, Some(&plugins)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let bytes_graph = load_from_bytes(&bytes, Some(&plugins)).unwrap();

        // Both should produce graphs with the same node/edge counts
        assert_eq!(path_graph.node_count(), bytes_graph.node_count());
        assert_eq!(path_graph.edge_count(), bytes_graph.edge_count());
    }

    // ------------------------------------------------------------------
    // Phase 1 P1U07: V7 legacy read path with upconvert
    // ------------------------------------------------------------------

    /// Helper: writes a V7-format blob programmatically (no provenance fields
    /// in the data section, MAGIC_BYTES V7 in the header). This is the
    /// "frozen V7 writer shim" described in the Phase 1 test plan; it lives
    /// in test code only, not in the production writer path.
    fn write_v7_fixture(path: &Path, graph: &CodeGraph) -> Result<(), PersistenceError> {
        let snapshot = graph.snapshot();
        let forward_stats = snapshot.edges().stats().forward;
        let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
        let header = GraphHeader::new(
            snapshot.nodes().len(),
            total_edges,
            snapshot.strings().len(),
            snapshot.files().len(),
        );

        // Serialize a V7-shaped data blob WITHOUT the provenance fields.
        // We use a local struct that exactly matches pre-Phase-1 GraphSnapshotData.
        #[derive(Serialize)]
        struct V7SnapshotData {
            nodes: NodeArena,
            edges: BidirectionalEdgeStore,
            strings: StringInterner,
            files: FileRegistry,
            indices: AuxiliaryIndices,
            macro_metadata: NodeMetadataStore,
        }
        let v7_data = V7SnapshotData {
            nodes: snapshot.nodes().clone(),
            edges: snapshot.edges().clone(),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: snapshot.macro_metadata().clone(),
        };

        let header_bytes = postcard::to_allocvec(&header)?;
        let data_bytes = postcard::to_allocvec(&v7_data)?;

        let mut file = File::create(path)?;
        file.write_all(MAGIC_BYTES)?; // V7 magic
        file.write_all(
            &u32::try_from(header_bytes.len())
                .expect("header fits in u32")
                .to_le_bytes(),
        )?;
        file.write_all(&header_bytes)?;
        file.write_all(&(data_bytes.len() as u64).to_le_bytes())?;
        file.write_all(&data_bytes)?;
        file.flush()?;
        Ok(())
    }

    #[test]
    fn phase1_v7_legacy_loads_with_defaulted_provenance() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();

        write_v7_fixture(temp_file.path(), &graph).unwrap();

        // Load via load_from_path (V7 dispatch + upconvert)
        let loaded = load_from_path(temp_file.path(), None).unwrap();

        // Topology is preserved
        assert_eq!(loaded.node_count(), graph.node_count());
        assert_eq!(loaded.edge_count(), graph.edge_count());
    }

    #[test]
    fn phase1_v7_legacy_loads_via_bytes() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();

        write_v7_fixture(temp_file.path(), &graph).unwrap();

        let bytes = std::fs::read(temp_file.path()).unwrap();
        let loaded = load_from_bytes(&bytes, None).unwrap();

        assert_eq!(loaded.node_count(), graph.node_count());
        assert_eq!(loaded.edge_count(), graph.edge_count());
    }

    #[test]
    fn phase1_v7_validate_snapshot_accepts_legacy() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();

        write_v7_fixture(temp_file.path(), &graph).unwrap();

        assert!(validate_snapshot(temp_file.path()).unwrap());
    }

    #[test]
    fn phase1_v8_round_trip_preserves_fact_epoch() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();

        // Save V8 — stamps a fact_epoch
        save_to_path(&graph, temp_file.path()).unwrap();

        // Load header — fact_epoch should be > 0
        let header = load_header_from_path(temp_file.path()).unwrap();
        assert!(
            header.fact_epoch() > 0,
            "V8 save should stamp a non-zero fact_epoch"
        );
    }

    #[test]
    fn phase1_repeated_saves_produce_increasing_epochs() {
        let graph = CodeGraph::new();
        let temp_file = NamedTempFile::new().unwrap();

        save_to_path(&graph, temp_file.path()).unwrap();
        let epoch1 = load_header_from_path(temp_file.path())
            .unwrap()
            .fact_epoch();

        save_to_path(&graph, temp_file.path()).unwrap();
        let epoch2 = load_header_from_path(temp_file.path())
            .unwrap()
            .fact_epoch();

        assert!(
            epoch2 > epoch1,
            "second save epoch ({epoch2}) must exceed first ({epoch1})"
        );
    }

    #[test]
    fn stamp_file_indexed_at_covers_sparse_registry() {
        // Register 5 files at slots 1..=5, then unregister slots 2 and 3
        // to create sparsity. stamp_file_indexed_at must still reach slot 4
        // and 5 even though len() == 3 after unregistration.
        let mut reg = FileRegistry::new();

        let id1 = reg.register(Path::new("/a.rs")).unwrap();
        let id2 = reg.register(Path::new("/b.rs")).unwrap();
        let id3 = reg.register(Path::new("/c.rs")).unwrap();
        let id4 = reg.register(Path::new("/d.rs")).unwrap();
        let id5 = reg.register(Path::new("/e.rs")).unwrap();

        // Unregister low slots to create sparsity
        reg.unregister(id2);
        reg.unregister(id3);

        // len() is now 3, but slot_count() should be 6 (sentinel + 5 slots)
        assert_eq!(reg.len(), 3);
        assert_eq!(reg.slot_count(), 6);

        // Stamp all live files
        stamp_file_indexed_at(&mut reg, 42_000);

        // Every live file must have the epoch, including high-slot files
        assert_eq!(reg.file_provenance(id1).unwrap().indexed_at, 42_000);
        assert_eq!(reg.file_provenance(id4).unwrap().indexed_at, 42_000);
        assert_eq!(reg.file_provenance(id5).unwrap().indexed_at, 42_000);

        // Vacant slots must return None (no provenance view)
        assert!(reg.file_provenance(id2).is_none());
        assert!(reg.file_provenance(id3).is_none());
    }

    #[test]
    fn stamp_file_indexed_at_covers_reused_slots() {
        // After unregistering and re-registering, free-list reuse should
        // not break stamping for any live slot.
        let mut reg = FileRegistry::new();

        let id1 = reg.register(Path::new("/first.rs")).unwrap();
        let id2 = reg.register(Path::new("/second.rs")).unwrap();
        let id3 = reg.register(Path::new("/third.rs")).unwrap();

        // Unregister slot 2, then register a new file that reuses the slot
        reg.unregister(id2);
        let id_reused = reg.register(Path::new("/reused.rs")).unwrap();

        // The reused slot should recycle the old index
        assert_eq!(id_reused.index(), id2.index());

        // Stamp all
        stamp_file_indexed_at(&mut reg, 99_000);

        // All live files (including the one in the reused slot) must be stamped
        assert_eq!(reg.file_provenance(id1).unwrap().indexed_at, 99_000);
        assert_eq!(reg.file_provenance(id_reused).unwrap().indexed_at, 99_000);
        assert_eq!(reg.file_provenance(id3).unwrap().indexed_at, 99_000);
    }

    #[test]
    fn provenance_first_seen_survives_save_load_save_round_trip() {
        // save → load → save → reload: first_seen_epoch from the first save
        // must survive the second save.
        let graph = graph_with_one_node("my_module::my_fn", Language::Rust, Path::new("/test.rs"));
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // First save
        save_to_path(&graph, path).unwrap();
        let header1 = load_header_from_path(path).unwrap();
        let epoch1 = header1.fact_epoch();
        assert!(epoch1 > 0, "first save must stamp a non-zero epoch");

        // Load: graph now carries provenance with first_seen == epoch1
        let loaded = load_from_path(path, Some(&plugins)).unwrap();

        // Verify node provenance was loaded
        let snap1 = loaded.snapshot();
        let node_id = snap1.nodes().iter().next().unwrap().0;
        let prov1 = snap1.node_provenance(node_id).unwrap();
        assert_eq!(prov1.first_seen_epoch, epoch1);
        assert_eq!(prov1.last_seen_epoch, epoch1);

        // Second save: must preserve first_seen_epoch
        save_to_path(&loaded, path).unwrap();
        let header2 = load_header_from_path(path).unwrap();
        let epoch2 = header2.fact_epoch();
        assert!(epoch2 > epoch1, "second epoch must exceed first");

        // Reload and verify first_seen survived
        let reloaded = load_from_path(path, Some(&plugins)).unwrap();
        let snap2 = reloaded.snapshot();
        let node_id2 = snap2.nodes().iter().next().unwrap().0;
        let prov2 = snap2.node_provenance(node_id2).unwrap();
        assert_eq!(
            prov2.first_seen_epoch, epoch1,
            "first_seen_epoch must survive save/load/save round-trip"
        );
        assert_eq!(
            prov2.last_seen_epoch, epoch2,
            "last_seen_epoch must advance to the second save epoch"
        );
    }

    #[test]
    fn provenance_content_hash_refreshed_on_resave() {
        // Preserved provenance must still update content_hash to the current
        // node body hash, not carry a stale hash from the prior save.
        let graph = graph_with_one_node(
            "my_module::hash_fn",
            Language::Rust,
            Path::new("/hash_test.rs"),
        );
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // First save
        save_to_path(&graph, path).unwrap();

        // Load
        let loaded = load_from_path(path, Some(&plugins)).unwrap();
        let snap1 = loaded.snapshot();
        let node_id = snap1.nodes().iter().next().unwrap().0;
        let hash1 = snap1.node_provenance(node_id).unwrap().content_hash;

        // Second save
        save_to_path(&loaded, path).unwrap();

        // Reload and verify the content hash is still present (not zeroed)
        let reloaded = load_from_path(path, Some(&plugins)).unwrap();
        let snap2 = reloaded.snapshot();
        let node_id2 = snap2.nodes().iter().next().unwrap().0;
        let hash2 = snap2.node_provenance(node_id2).unwrap().content_hash;
        assert_eq!(
            hash1, hash2,
            "content_hash must be refreshed from current node body on resave"
        );
    }

    #[test]
    fn edge_provenance_first_seen_survives_round_trip() {
        // Edge provenance must also preserve first_seen_epoch across
        // load/save round-trips where edge slot identity is unchanged.
        use crate::graph::unified::edge::{EdgeKind, ResolvedVia};

        let mut graph = graph_with_one_node(
            "my_module::caller",
            Language::Rust,
            Path::new("/edge_test.rs"),
        );

        // Add a second node and an edge between them
        let file_id = graph.files().get(Path::new("/edge_test.rs")).unwrap();
        let name2 = graph.strings_mut().intern("callee").unwrap();
        let qname2 = graph.strings_mut().intern("my_module::callee").unwrap();
        let entry2 = NodeEntry::new(NodeKind::Function, name2, file_id)
            .with_location(5, 0, 5, 10)
            .with_qualified_name(qname2);
        let node2 = graph.nodes_mut().alloc(entry2).unwrap();

        let node1 = graph.nodes().iter().next().unwrap().0;
        let _edge = graph.edges().add_edge(
            node1,
            node2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // First save
        save_to_path(&graph, path).unwrap();
        let epoch1 = load_header_from_path(path).unwrap().fact_epoch();

        // Load → resave
        let loaded = load_from_path(path, Some(&plugins)).unwrap();
        save_to_path(&loaded, path).unwrap();
        let epoch2 = load_header_from_path(path).unwrap().fact_epoch();
        assert!(epoch2 > epoch1);

        // Reload and check edge provenance
        let reloaded = load_from_path(path, Some(&plugins)).unwrap();
        let snap = reloaded.snapshot();

        // Find an edge and check its provenance
        let n1 = snap.nodes().iter().next().unwrap().0;
        let edges = snap.edges().edges_from(n1);
        assert!(!edges.is_empty(), "graph must have at least one edge");
        drop(edges);

        // Scan all edge slots to find one with provenance and verify it
        // preserves first_seen_epoch. We iterate all slots rather than
        // guessing slot 0, because CSR layout may assign any index.
        let forward_stats = snap.edges().stats().forward;
        let total_edges = forward_stats.csr_edge_count + forward_stats.delta_edge_count;
        let mut found_preserved = false;
        for idx in 0..total_edges {
            if let Ok(i) = u32::try_from(idx) {
                let eid = crate::graph::unified::edge::id::EdgeId::new(i);
                if let Some(eprov) = snap.edge_provenance(eid) {
                    assert_eq!(
                        eprov.first_seen_epoch, epoch1,
                        "edge slot {idx}: first_seen_epoch must survive round-trip"
                    );
                    assert_eq!(
                        eprov.last_seen_epoch, epoch2,
                        "edge slot {idx}: last_seen_epoch must advance to second epoch"
                    );
                    found_preserved = true;
                }
            }
        }
        assert!(
            found_preserved,
            "must find at least one edge with preserved provenance"
        );
    }

    #[test]
    fn provenance_reused_node_slot_gets_fresh_first_seen() {
        // Load a graph that carries provenance (first_seen == epoch1),
        // remove a node to free its slot, allocate a new node in the same
        // slot (bumped generation), then save. The new occupant must get
        // first_seen == epoch2, not carry over epoch1 from the old tenant.
        let graph = graph_with_one_node(
            "my_module::original",
            Language::Rust,
            Path::new("/reuse_test.rs"),
        );
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();
        let plugins = create_test_plugin_manager();

        // First save
        save_to_path(&graph, path).unwrap();
        let epoch1 = load_header_from_path(path).unwrap().fact_epoch();

        // Load — graph now carries provenance with first_seen == epoch1
        let mut loaded = load_from_path(path, Some(&plugins)).unwrap();

        // Find the existing node and its slot index
        let (old_node_id, _) = loaded.nodes().iter().next().unwrap();
        let old_index = old_node_id.index();
        let old_generation = old_node_id.generation();

        // Verify provenance exists for the old node
        assert!(
            loaded.node_provenance(old_node_id).is_some(),
            "loaded graph must carry provenance for the original node"
        );

        // Remove the old node to free the slot
        let file_id = loaded.files().get(Path::new("/reuse_test.rs")).unwrap();
        loaded.nodes_mut().remove(old_node_id);

        // Allocate a new node — should reuse the same slot with bumped generation
        let name2 = loaded.strings_mut().intern("replacement").unwrap();
        let qname2 = loaded
            .strings_mut()
            .intern("my_module::replacement")
            .unwrap();
        let entry2 = NodeEntry::new(NodeKind::Function, name2, file_id)
            .with_location(10, 0, 10, 20)
            .with_qualified_name(qname2);
        let new_node_id = loaded.nodes_mut().alloc(entry2).unwrap();

        // Confirm slot reuse with bumped generation
        assert_eq!(
            new_node_id.index(),
            old_index,
            "new node must reuse the freed slot"
        );
        assert!(
            new_node_id.generation() > old_generation,
            "reused slot must have a bumped generation"
        );

        // Second save
        save_to_path(&loaded, path).unwrap();
        let epoch2 = load_header_from_path(path).unwrap().fact_epoch();
        assert!(epoch2 > epoch1);

        // Reload and verify the new occupant has fresh provenance
        let reloaded = load_from_path(path, Some(&plugins)).unwrap();
        let snap = reloaded.snapshot();
        let (reloaded_id, _) = snap.nodes().iter().next().unwrap();

        let prov = snap
            .node_provenance(reloaded_id)
            .expect("new occupant must have provenance");
        assert_eq!(
            prov.first_seen_epoch, epoch2,
            "reused slot with bumped generation must get fresh first_seen_epoch, \
             not carry over from the old tenant"
        );
        assert_eq!(prov.last_seen_epoch, epoch2);
    }

    // ------------------------------------------------------------------
    // U03 — V11 snapshot bump + V10 → V11 upconvert (DESIGN §10.3)
    //
    // Coverage:
    //   - `u03_upconvert_v10_to_v11_maps_typed_metadata`: every
    //     `StoredEntry` shape (Macro typed payload, Classpath typed
    //     payload, synthetic-only flag) round-trips byte-for-byte through
    //     the envelope upconvert. Per DESIGN §10.3, the per-element
    //     reshape is owned by `NodeMetadataStore`'s serde impl (U02), so
    //     this test asserts the envelope upconvert is a no-op on the
    //     store value itself plus the additive `c_indirect_tables =
    //     None`.
    //   - `u03_v11_writer_v11_reader_round_trip`: write a small synthetic
    //     graph through the V11 writer and read it back. Both magic
    //     dispatch (V11 arm) and the in-memory shape are exercised.
    //   - `u03_v10_payload_triggers_upconvert`: write a hand-crafted V10
    //     payload (using the V10 writer) and read it back through the
    //     V11 loader — asserts the upconvert path is wired into the
    //     dispatch and produces a usable `CodeGraph`.
    //   - `u03_magic_dispatch_routes_v11_buffer_to_v11`: a buffer prefixed
    //     with `SQRY_GRAPH_V11` resolves to `FormatVersion::V11`, not V10.
    //     Belt-and-braces over the parallel test in `format.rs`.
    // ------------------------------------------------------------------

    use crate::graph::unified::storage::{
        ClasspathNodeMetadata, MacroNodeMetadata, NodeFlags, NodeMetadataStore, StoredEntry,
        TypedMetadata,
    };

    /// Build a sample `NodeMetadataStore` carrying one entry of each shape
    /// the V10 → V11 upconvert needs to handle:
    ///   - typed-Macro + EMPTY flags (legacy `NodeMetadata::Macro`)
    ///   - typed-Classpath + EMPTY flags (legacy `NodeMetadata::Classpath`)
    ///   - no typed payload + SYNTHETIC flag (legacy `NodeMetadata::Synthetic`)
    fn metadata_store_with_all_legacy_shapes() -> NodeMetadataStore {
        use crate::graph::unified::node::id::NodeId;

        let mut store = NodeMetadataStore::new();

        let macro_node = NodeId::new(1, 1);
        let macro_md = MacroNodeMetadata {
            macro_generated: Some(true),
            macro_source: Some("vec".to_string()),
            ..Default::default()
        };
        store.insert_entry(
            macro_node,
            StoredEntry {
                typed: Some(TypedMetadata::Macro(macro_md)),
                flags: NodeFlags::EMPTY,
            },
        );

        let classpath_node = NodeId::new(2, 1);
        let classpath_md = ClasspathNodeMetadata {
            coordinates: Some("com.google.guava:guava:33.0.0".to_string()),
            jar_path: "/tmp/guava.jar".to_string(),
            fqn: "com.google.common.collect.ImmutableList".to_string(),
            is_direct_dependency: true,
        };
        store.insert_entry(
            classpath_node,
            StoredEntry {
                typed: Some(TypedMetadata::Classpath(classpath_md)),
                flags: NodeFlags::EMPTY,
            },
        );

        let synthetic_node = NodeId::new(3, 1);
        store.insert_entry(
            synthetic_node,
            StoredEntry {
                typed: None,
                flags: NodeFlags::SYNTHETIC,
            },
        );

        store
    }

    /// Build a tiny `GraphSnapshotDataV10` populated with the three legacy
    /// metadata shapes. Used to drive the V10 → V11 upconvert + V10-payload
    /// reader tests.
    ///
    /// Translates the live `BidirectionalEdgeStore` / `NodeMetadataStore`
    /// produced by `CodeGraph::new` into the V10 wire-format mirrors
    /// ([`super::legacy_v10::BidirectionalEdgeStoreV10`] /
    /// [`super::legacy_v10::NodeMetadataStoreV10`]) so the test payload
    /// matches the on-disk V10 shape that
    /// [`upconvert_v10_to_v11`] now expects.
    fn build_v10_snapshot_for_upconvert() -> GraphSnapshotDataV10 {
        use super::super::legacy_v10::{
            translate_bidirectional_edge_store_v11_to_v10, translate_metadata_store_v11_to_v10,
        };
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let live_edges = snapshot.edges().clone();
        let live_metadata = metadata_store_with_all_legacy_shapes();
        GraphSnapshotDataV10 {
            nodes: snapshot.nodes().clone(),
            edges: translate_bidirectional_edge_store_v11_to_v10(&live_edges),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: translate_metadata_store_v11_to_v10(&live_metadata)
                .expect("legacy metadata store fits V10 wire format"),
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
            scope_arena: snapshot.scope_arena().clone(),
            alias_table: snapshot.alias_table().clone(),
            shadow_table: snapshot.shadow_table().clone(),
            scope_provenance: snapshot.scope_provenance_store().clone(),
            file_segments: snapshot.file_segments().clone(),
        }
    }

    /// V10 → V11 envelope upconvert preserves every metadata-store entry
    /// and stamps `c_indirect_tables = None`.
    ///
    /// Per DESIGN §10.3 the per-entry reshape (legacy `NodeMetadata::Macro`
    /// → `StoredEntry { typed: Macro, flags: EMPTY }`, ...) is performed by
    /// `NodeMetadataStore::Deserialize` (landed in U02). The envelope
    /// upconvert tested here must therefore be a no-op on the value plus
    /// the additive `c_indirect_tables` slot.
    #[test]
    fn u03_upconvert_v10_to_v11_maps_typed_metadata() {
        use crate::graph::unified::node::id::NodeId;

        let v10 = build_v10_snapshot_for_upconvert();
        // V10 wire-format metadata uses NodeMetadataStoreV10 — capture the
        // entries we expect to round-trip before the translate consumes the
        // value.
        let v10_entries: Vec<((u32, u64), u8, bool)> = v10
            .macro_metadata
            .entries
            .iter()
            .map(|e| {
                (
                    (e.index, e.generation),
                    e.kind,
                    e.macro_data.is_some() || e.classpath_data.is_some(),
                )
            })
            .collect();

        let v11 = upconvert_v10_to_v11(v10).expect("V10 → V11 upconvert");

        // `c_indirect_tables` slot defaulted to None.
        assert!(
            v11.c_indirect_tables.is_none(),
            "c_indirect_tables must default to None on V10 → V11 upconvert"
        );

        // Every V10 metadata entry round-tripped to a V11 `StoredEntry`.
        // The captured `v10_entries` tuple is ((index, gen), kind_byte,
        // has_payload); spot-check counts match and each V10 kind maps to
        // a V11 entry the live store can resolve.
        let v11_count = v11.macro_metadata.iter_entries().count();
        assert_eq!(
            v11_count,
            v10_entries.len(),
            "envelope upconvert must preserve entry count"
        );
        for ((index, generation), v10_kind, has_payload) in v10_entries.iter().copied() {
            let node = NodeId::new(index, generation);
            let v11_typed = v11.macro_metadata.get_typed(node).cloned();
            let v11_flags = v11.macro_metadata.get_flags(node);
            // V10 kind discriminants: 0 Macro / 1 Classpath / 2 Synthetic.
            match v10_kind {
                0 => {
                    assert!(has_payload, "V10 Macro entry must carry macro_data");
                    assert!(
                        matches!(v11_typed, Some(TypedMetadata::Macro(_))),
                        "V10 Macro must upconvert to TypedMetadata::Macro",
                    );
                    assert_eq!(v11_flags.bits(), NodeFlags::EMPTY.bits());
                }
                1 => {
                    assert!(has_payload, "V10 Classpath entry must carry classpath_data");
                    assert!(
                        matches!(v11_typed, Some(TypedMetadata::Classpath(_))),
                        "V10 Classpath must upconvert to TypedMetadata::Classpath",
                    );
                    assert_eq!(v11_flags.bits(), NodeFlags::EMPTY.bits());
                }
                2 => {
                    assert!(
                        v11_typed.is_none(),
                        "V10 Synthetic carries no typed payload"
                    );
                    assert!(
                        v11_flags.contains(NodeFlags::SYNTHETIC),
                        "V10 Synthetic must surface as the SYNTHETIC flag",
                    );
                }
                other => panic!("unexpected V10 kind discriminant {other}"),
            }
        }

        // Spot-check each shape by NodeId. Anchors regression coverage
        // for the three legacy `NodeMetadata` variants the design calls
        // out:
        //   Macro(m)   → typed = Some(Macro(m)), flags = EMPTY
        //   Classpath(c) → typed = Some(Classpath(c)), flags = EMPTY
        //   Synthetic  → typed = None,         flags = SYNTHETIC
        let macro_node = NodeId::new(1, 1);
        match v11.macro_metadata.get_typed(macro_node) {
            Some(TypedMetadata::Macro(m)) => {
                assert_eq!(m.macro_generated, Some(true));
                assert_eq!(m.macro_source.as_deref(), Some("vec"));
            }
            other => panic!("expected Macro typed metadata, got {other:?}"),
        }
        assert_eq!(v11.macro_metadata.get_flags(macro_node), NodeFlags::EMPTY);

        let classpath_node = NodeId::new(2, 1);
        match v11.macro_metadata.get_typed(classpath_node) {
            Some(TypedMetadata::Classpath(c)) => {
                assert_eq!(c.jar_path, "/tmp/guava.jar");
                assert!(c.is_direct_dependency);
            }
            other => panic!("expected Classpath typed metadata, got {other:?}"),
        }
        assert_eq!(
            v11.macro_metadata.get_flags(classpath_node),
            NodeFlags::EMPTY,
        );

        let synthetic_node = NodeId::new(3, 1);
        assert!(
            v11.macro_metadata.get_typed(synthetic_node).is_none(),
            "synthetic-only entry must carry no typed payload",
        );
        assert!(
            v11.macro_metadata
                .get_flags(synthetic_node)
                .contains(NodeFlags::SYNTHETIC),
            "synthetic-only entry must carry the SYNTHETIC flag",
        );
    }

    /// Writing a graph via `save_to_path` (V12 magic, Phase β joint-stubs)
    /// and reading it back through `load_from_path` round-trips the graph
    /// state and exercises the V12 reader-arm dispatch.
    ///
    /// Originally a V11 round-trip (Phase A C-icall U03); promoted to V12
    /// when the joint-stubs PR bumped the writer.
    #[test]
    fn u03_current_writer_current_reader_round_trip() {
        let mut graph = CodeGraph::new();
        // Seed a single function so the snapshot has non-trivial content
        // and the metadata-store wire format gets exercised end-to-end.
        {
            let file_id = graph
                .files_mut()
                .register_with_language(
                    std::path::Path::new("/tmp/u03.c"),
                    Some(crate::graph::node::Language::C),
                )
                .expect("register file");
            let name_id = graph
                .strings_mut()
                .intern("u03_target")
                .expect("intern name");
            let qname_id = graph
                .strings_mut()
                .intern("u03_target")
                .expect("intern qname");
            let entry = NodeEntry::new(NodeKind::Function, name_id, file_id)
                .with_location(1, 0, 1, 10)
                .with_qualified_name(qname_id);
            let node_id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");
            graph.indices_mut().add(
                node_id,
                entry.kind,
                entry.name,
                entry.qualified_name,
                entry.file,
            );
        }

        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        save_to_path(&graph, path).expect("save_to_path");

        // Verify magic on disk is V13 (T3 error chains — current writer).
        let raw = std::fs::read(path).expect("read snapshot bytes");
        assert_eq!(
            &raw[..MAGIC_BYTES_V13.len()],
            MAGIC_BYTES_V13.as_slice(),
            "current writer must stamp V13 magic on disk",
        );

        // Header version must be V13.
        let header = load_header_from_path(path).expect("load header");
        assert_eq!(header.version, FormatVersion::V13.as_u32());

        // Loader returns a usable graph (V13 arm in `load_from_path`).
        let plugins = create_test_plugin_manager();
        let loaded = load_from_path(path, Some(&plugins)).expect("load V13");
        let snap = loaded.snapshot();
        assert_eq!(snap.nodes().len(), 1, "node count must round-trip");
        // Strings round-trip too (the seed function carries one interned name).
        assert!(
            !snap.strings().is_empty(),
            "interned name must survive V13 round-trip",
        );
        // Phase β joint-stubs land empty by default — no resolver populates
        // them today.
        assert!(
            snap.macro_metadata().framework_routes().is_empty(),
            "framework_routes must be empty in the stub",
        );
        assert!(
            snap.macro_metadata().dispatch_tables().is_empty(),
            "dispatch_tables must be empty in the stub",
        );
    }

    /// Hand-craft a V10 payload (V10 magic + V10 envelope, written by the
    /// legacy V10 writer) and read it back through the current (V12) loader.
    /// Asserts the V10 reader arm + the upconvert chain
    /// `upconvert_v10_to_v11` → `upconvert_v11_to_v12` are wired into
    /// dispatch.
    ///
    /// Uses `write_framed_v10` directly so this test is independent of any
    /// future shift in the writer's default magic.
    #[test]
    fn u03_v10_payload_triggers_upconvert() {
        let v10 = build_v10_snapshot_for_upconvert();

        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = FormatVersion::V10.as_u32();

        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        {
            let file = File::create(path).expect("create temp file");
            let mut writer = BufWriter::new(file);
            write_framed_v10(&mut writer, &header, &v10).expect("write V10 frame");
            writer.flush().expect("flush V10");
        }

        // Magic dispatch sees V10; the V12 loader chains the upconvert
        // through V10 → V11 → V12.
        let plugins = create_test_plugin_manager();
        let loaded = load_from_path(path, Some(&plugins))
            .expect("V12 loader must accept a V10 payload via chained upconvert");

        // The loaded graph carries all three legacy metadata shapes in the
        // new `StoredEntry` representation.
        use crate::graph::unified::node::id::NodeId;
        let store = loaded.snapshot().macro_metadata().clone();
        let macro_node = NodeId::new(1, 1);
        let classpath_node = NodeId::new(2, 1);
        let synthetic_node = NodeId::new(3, 1);

        assert!(matches!(
            store.get_typed(macro_node),
            Some(TypedMetadata::Macro(_))
        ));
        assert!(matches!(
            store.get_typed(classpath_node),
            Some(TypedMetadata::Classpath(_))
        ));
        assert!(store.get_typed(synthetic_node).is_none());
        assert!(
            store
                .get_flags(synthetic_node)
                .contains(NodeFlags::SYNTHETIC)
        );
    }

    /// Belt-and-braces: a raw buffer prefixed with `SQRY_GRAPH_V11`
    /// resolves to `FormatVersion::V11`, not V10. Mirrors the
    /// `phase_a_format_version_dispatch_v11_before_v10` test in `format.rs`
    /// but anchored to the snapshot module so a refactor of either side
    /// can't silently regress dispatch ordering.
    #[test]
    fn u03_magic_dispatch_routes_v11_buffer_to_v11() {
        let mut buf = MAGIC_BYTES_V11.to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(
            FormatVersion::from_magic(&buf),
            Some(FormatVersion::V11),
            "buffer prefixed with V11 magic must dispatch to V11",
        );
    }

    /// Phase β joint-stubs: writing a V11 frame on disk and loading it
    /// through `load_from_path` exercises the `upconvert_v11_to_v12`
    /// inline path, returns a usable graph, and zero-initialises both
    /// joint-stub fields on the in-memory metadata store.
    #[test]
    fn phase_beta_v11_payload_upconverts_to_v12() {
        // Construct a minimal V11 envelope. We reuse the V11 writer (kept
        // alive behind `#[allow(dead_code)]` for exactly this purpose).
        let v11 = GraphSnapshotDataV11 {
            nodes: NodeArena::default(),
            edges: BidirectionalEdgeStore::default(),
            strings: StringInterner::new(),
            files: FileRegistry::default(),
            indices: AuxiliaryIndices::default(),
            macro_metadata: NodeMetadataStore::default(),
            node_provenance: NodeProvenanceStore::default(),
            edge_provenance: EdgeProvenanceStore::default(),
            scope_arena: ScopeArena::default(),
            alias_table: AliasTable::default(),
            shadow_table: ShadowTable::default(),
            scope_provenance: ScopeProvenanceStore::default(),
            file_segments: FileSegmentTable::default(),
            c_indirect_tables: None,
        };

        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = FormatVersion::V11.as_u32();

        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        {
            let file = File::create(path).expect("create temp file");
            let mut writer = BufWriter::new(file);
            write_framed_v11(&mut writer, &header, &v11).expect("write V11 frame");
            writer.flush().expect("flush V11");
        }

        // Magic dispatch sees V11; the V12 loader runs the upconvert.
        let plugins = create_test_plugin_manager();
        let loaded = load_from_path(path, Some(&plugins))
            .expect("V12 loader must accept a V11 payload via upconvert_v11_to_v12");
        let snap = loaded.snapshot();
        assert!(
            snap.macro_metadata().framework_routes().is_empty(),
            "framework_routes must be empty after V11 → V12 upconvert",
        );
        assert!(
            snap.macro_metadata().dispatch_tables().is_empty(),
            "dispatch_tables must be empty after V11 → V12 upconvert",
        );
    }

    /// Phase β joint-stubs: V12 magic dispatch routes a V12-prefixed buffer
    /// to `FormatVersion::V12`, not V11 or V10. Mirrors the V11/V10 guards
    /// for the new format.
    #[test]
    fn phase_beta_magic_dispatch_routes_v12_buffer_to_v12() {
        let mut buf = MAGIC_BYTES_V12.to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(
            FormatVersion::from_magic(&buf),
            Some(FormatVersion::V12),
            "buffer prefixed with V12 magic must dispatch to V12",
        );
    }

    // ------------------------------------------------------------------
    // U03 codex iter-1 regression coverage — versioned V10 wire types.
    //
    // These tests pin the V10 wire format (`EdgeKindV10`,
    // `NodeMetadataV10`/`NodeMetadataEntryV10`/`NodeMetadataStoreV10`) so
    // future live-type changes (U02 `NodeFlags`, U04
    // `EdgeKind::Calls.resolved_via`, ...) can NOT silently shift the V10
    // on-disk layout. Every test hand-crafts a V10 frame via the legacy
    // V10 writer + V10 wire-type translators in
    // `super::super::legacy_v10`, runs it through `load_from_path`, and
    // asserts the V11 reader sees the expected live shape after upconvert.
    // ------------------------------------------------------------------

    /// V10 `Calls` edge with `argument_count=7, is_async=true` round-trips
    /// through the V10 writer → V10 reader → `upconvert_v10_to_v11` chain
    /// into a live `EdgeKind::Calls` carrying the same fields.
    ///
    /// U04 will add `resolved_via: ResolvedVia` to `EdgeKind::Calls`; the
    /// `translate_edge_v10_to_v11` Calls arm carries the integration TODO
    /// that documents the one-line fix at that time (stamp
    /// `ResolvedVia::Direct`). This test does not assert on `resolved_via`
    /// today because the field does not yet exist on the live enum; once
    /// U04 lands, extend this test to assert `ResolvedVia::Direct` for
    /// any V10 `Calls` edge.
    #[test]
    fn u03_v10_calls_edge_postcard_old_wire_roundtrips() {
        use super::super::legacy_v10::{
            translate_bidirectional_edge_store_v11_to_v10, translate_metadata_store_v11_to_v10,
        };
        use crate::graph::unified::edge::{EdgeKind, ResolvedVia};

        // 1. Build a live graph that carries exactly one Calls edge with
        //    distinctive (argument_count, is_async) so we can spot-check
        //    after upconvert.
        let mut graph = CodeGraph::new();
        let file_id = graph
            .files_mut()
            .register_with_language(
                std::path::Path::new("/tmp/u03_calls.c"),
                Some(crate::graph::node::Language::C),
            )
            .expect("register file");
        let caller_name = graph.strings_mut().intern("caller").expect("intern caller");
        let callee_name = graph.strings_mut().intern("callee").expect("intern callee");
        let caller_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, caller_name, file_id))
            .expect("alloc caller");
        let callee_id = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Function, callee_name, file_id))
            .expect("alloc callee");
        graph.edges_mut().add_edge(
            caller_id,
            callee_id,
            EdgeKind::Calls {
                argument_count: 7,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        // 2. Snap live → V10 wire types.
        let snapshot = graph.snapshot();
        let v10 = GraphSnapshotDataV10 {
            nodes: snapshot.nodes().clone(),
            edges: translate_bidirectional_edge_store_v11_to_v10(snapshot.edges()),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: translate_metadata_store_v11_to_v10(snapshot.macro_metadata())
                .expect("metadata fits V10 wire"),
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
            scope_arena: snapshot.scope_arena().clone(),
            alias_table: snapshot.alias_table().clone(),
            shadow_table: snapshot.shadow_table().clone(),
            scope_provenance: snapshot.scope_provenance_store().clone(),
            file_segments: snapshot.file_segments().clone(),
        };

        // 3. Write a V10 frame.
        let mut header = GraphHeader::new(2, 1, 2, 1);
        header.version = FormatVersion::V10.as_u32();
        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        {
            let file = File::create(path).expect("create temp file");
            let mut writer = BufWriter::new(file);
            write_framed_v10(&mut writer, &header, &v10).expect("write V10 frame");
            writer.flush().expect("flush V10");
        }

        // 4. Read back through the V11 loader. The V10 → V11 upconvert
        //    must translate the V10 `Calls { argument_count: 7, is_async: true }`
        //    into a live `EdgeKind::Calls` carrying the same fields.
        let plugins = create_test_plugin_manager();
        let loaded =
            load_from_path(path, Some(&plugins)).expect("V11 loader must accept V10 Calls payload");
        let loaded_snap = loaded.snapshot();
        let edges = loaded_snap.edges().edges_from(caller_id);
        assert_eq!(
            edges.len(),
            1,
            "exactly one Calls edge must survive round-trip"
        );
        match &edges[0].kind {
            EdgeKind::Calls {
                argument_count,
                is_async,
                resolved_via,
            } => {
                assert_eq!(
                    *argument_count, 7,
                    "argument_count must round-trip from V10 wire"
                );
                assert!(*is_async, "is_async must round-trip from V10 wire");
                assert_eq!(
                    *resolved_via,
                    ResolvedVia::Direct,
                    "V10 Calls edges must upconvert with resolved_via = Direct (V10 predates indirect-call resolver)"
                );
            }
            other => panic!("expected EdgeKind::Calls after upconvert, got {other:?}"),
        }
    }

    /// V10 `NodeMetadataV10::Macro` (legacy three-variant enum, no `flags`
    /// byte on the entry) round-trips through the V10 writer →
    /// `upconvert_v10_to_v11` into a live
    /// `StoredEntry { typed: Some(TypedMetadata::Macro(_)), flags: NodeFlags::EMPTY }`.
    #[test]
    fn u03_v10_metadata_legacy_macro_postcard_roundtrips() {
        use super::super::legacy_v10::{
            LEGACY_V7_KIND_MACRO, NodeMetadataEntryV10, NodeMetadataStoreV10,
            translate_bidirectional_edge_store_v11_to_v10,
        };
        use crate::graph::unified::node::id::NodeId;

        // Hand-craft a V10 metadata store with one legacy Macro entry.
        let v10_store = NodeMetadataStoreV10 {
            entries: vec![NodeMetadataEntryV10 {
                index: 5,
                generation: 1,
                kind: LEGACY_V7_KIND_MACRO,
                macro_data: Some(MacroNodeMetadata {
                    macro_generated: Some(true),
                    macro_source: Some("println".to_string()),
                    ..Default::default()
                }),
                classpath_data: None,
            }],
        };

        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let v10 = GraphSnapshotDataV10 {
            nodes: snapshot.nodes().clone(),
            edges: translate_bidirectional_edge_store_v11_to_v10(snapshot.edges()),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: v10_store,
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
            scope_arena: snapshot.scope_arena().clone(),
            alias_table: snapshot.alias_table().clone(),
            shadow_table: snapshot.shadow_table().clone(),
            scope_provenance: snapshot.scope_provenance_store().clone(),
            file_segments: snapshot.file_segments().clone(),
        };

        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = FormatVersion::V10.as_u32();
        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        {
            let file = File::create(path).expect("create temp file");
            let mut writer = BufWriter::new(file);
            write_framed_v10(&mut writer, &header, &v10).expect("write V10 frame");
            writer.flush().expect("flush V10");
        }

        let plugins = create_test_plugin_manager();
        let loaded = load_from_path(path, Some(&plugins))
            .expect("V11 loader must accept V10 legacy-Macro metadata payload");
        let store = loaded.snapshot().macro_metadata().clone();
        let macro_node = NodeId::new(5, 1);
        match store.get_typed(macro_node) {
            Some(TypedMetadata::Macro(m)) => {
                assert_eq!(m.macro_generated, Some(true));
                assert_eq!(m.macro_source.as_deref(), Some("println"));
            }
            other => panic!("expected TypedMetadata::Macro after V10 upconvert, got {other:?}"),
        }
        assert_eq!(
            store.get_flags(macro_node),
            NodeFlags::EMPTY,
            "legacy Macro entry must surface with EMPTY flags after V10 upconvert",
        );
    }

    /// V10 `NodeMetadataV10::Classpath` round-trips through the V10
    /// writer → `upconvert_v10_to_v11` into a live
    /// `StoredEntry { typed: Some(TypedMetadata::Classpath(_)), flags: NodeFlags::EMPTY }`.
    #[test]
    fn u03_v10_metadata_legacy_classpath_postcard_roundtrips() {
        use super::super::legacy_v10::{
            LEGACY_V7_KIND_CLASSPATH, NodeMetadataEntryV10, NodeMetadataStoreV10,
            translate_bidirectional_edge_store_v11_to_v10,
        };
        use crate::graph::unified::node::id::NodeId;

        let v10_store = NodeMetadataStoreV10 {
            entries: vec![NodeMetadataEntryV10 {
                index: 9,
                generation: 1,
                kind: LEGACY_V7_KIND_CLASSPATH,
                macro_data: None,
                classpath_data: Some(ClasspathNodeMetadata {
                    coordinates: Some("org.apache.commons:commons-lang3:3.14".to_string()),
                    jar_path: "/tmp/commons-lang3.jar".to_string(),
                    fqn: "org.apache.commons.lang3.StringUtils".to_string(),
                    is_direct_dependency: false,
                }),
            }],
        };

        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let v10 = GraphSnapshotDataV10 {
            nodes: snapshot.nodes().clone(),
            edges: translate_bidirectional_edge_store_v11_to_v10(snapshot.edges()),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: v10_store,
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
            scope_arena: snapshot.scope_arena().clone(),
            alias_table: snapshot.alias_table().clone(),
            shadow_table: snapshot.shadow_table().clone(),
            scope_provenance: snapshot.scope_provenance_store().clone(),
            file_segments: snapshot.file_segments().clone(),
        };

        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = FormatVersion::V10.as_u32();
        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        {
            let file = File::create(path).expect("create temp file");
            let mut writer = BufWriter::new(file);
            write_framed_v10(&mut writer, &header, &v10).expect("write V10 frame");
            writer.flush().expect("flush V10");
        }

        let plugins = create_test_plugin_manager();
        let loaded = load_from_path(path, Some(&plugins))
            .expect("V11 loader must accept V10 legacy-Classpath metadata payload");
        let store = loaded.snapshot().macro_metadata().clone();
        let classpath_node = NodeId::new(9, 1);
        match store.get_typed(classpath_node) {
            Some(TypedMetadata::Classpath(c)) => {
                assert_eq!(c.jar_path, "/tmp/commons-lang3.jar");
                assert_eq!(c.fqn, "org.apache.commons.lang3.StringUtils");
                assert!(!c.is_direct_dependency);
            }
            other => {
                panic!("expected TypedMetadata::Classpath after V10 upconvert, got {other:?}")
            }
        }
        assert_eq!(
            store.get_flags(classpath_node),
            NodeFlags::EMPTY,
            "legacy Classpath entry must surface with EMPTY flags after V10 upconvert",
        );
    }

    /// V10 `NodeMetadataV10::Synthetic` (unit variant, no payload)
    /// round-trips through the V10 writer → `upconvert_v10_to_v11` into a
    /// live `StoredEntry { typed: None, flags: NodeFlags::SYNTHETIC }`.
    /// This pins the spec-mandated three-variant V10 → V11 mapping for
    /// the variant U02 deliberately encodes via the new flag channel.
    #[test]
    fn u03_v10_metadata_legacy_synthetic_postcard_roundtrips() {
        use super::super::legacy_v10::{
            LEGACY_V7_KIND_SYNTHETIC, NodeMetadataEntryV10, NodeMetadataStoreV10,
            translate_bidirectional_edge_store_v11_to_v10,
        };
        use crate::graph::unified::node::id::NodeId;

        let v10_store = NodeMetadataStoreV10 {
            entries: vec![NodeMetadataEntryV10 {
                index: 11,
                generation: 1,
                kind: LEGACY_V7_KIND_SYNTHETIC,
                macro_data: None,
                classpath_data: None,
            }],
        };

        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let v10 = GraphSnapshotDataV10 {
            nodes: snapshot.nodes().clone(),
            edges: translate_bidirectional_edge_store_v11_to_v10(snapshot.edges()),
            strings: snapshot.strings().clone(),
            files: snapshot.files().clone(),
            indices: snapshot.indices().clone(),
            macro_metadata: v10_store,
            node_provenance: NodeProvenanceStore::new(),
            edge_provenance: EdgeProvenanceStore::new(),
            scope_arena: snapshot.scope_arena().clone(),
            alias_table: snapshot.alias_table().clone(),
            shadow_table: snapshot.shadow_table().clone(),
            scope_provenance: snapshot.scope_provenance_store().clone(),
            file_segments: snapshot.file_segments().clone(),
        };

        let mut header = GraphHeader::new(0, 0, 0, 0);
        header.version = FormatVersion::V10.as_u32();
        let temp_file = NamedTempFile::new().expect("tempfile");
        let path = temp_file.path();
        {
            let file = File::create(path).expect("create temp file");
            let mut writer = BufWriter::new(file);
            write_framed_v10(&mut writer, &header, &v10).expect("write V10 frame");
            writer.flush().expect("flush V10");
        }

        let plugins = create_test_plugin_manager();
        let loaded = load_from_path(path, Some(&plugins))
            .expect("V11 loader must accept V10 legacy-Synthetic metadata payload");
        let store = loaded.snapshot().macro_metadata().clone();
        let synthetic_node = NodeId::new(11, 1);
        assert!(
            store.get_typed(synthetic_node).is_none(),
            "legacy Synthetic carries no typed payload after V10 upconvert",
        );
        assert!(
            store
                .get_flags(synthetic_node)
                .contains(NodeFlags::SYNTHETIC),
            "legacy Synthetic must surface as the SYNTHETIC flag after V10 upconvert",
        );
    }
}
