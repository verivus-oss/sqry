//! Binary format definition for graph persistence.
//!
//! This module defines the on-disk format for persisted graphs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::manifest::ConfigProvenance;

/// Magic bytes identifying a sqry graph file (legacy alias for V7).
///
/// Version history:
/// - V1: Initial format (bincode)
/// - V2: Added config provenance support (bincode)
/// - V3: Added plugin version tracking (bincode)
/// - V4: Migrated to postcard serialization with length-prefixed framing
/// - V5: Added `HttpMethod::All` variant for wildcard endpoint matching
/// - V6: Added `NodeMetadataStore` for macro boundary analysis + `CfgGate` edge kind
/// - V7: Added classpath NodeKind/EdgeKind variants, `NodeMetadata` enum, `FileEntry.is_external`
/// - V8 (Phase 1 fact-layer hardening): Adds `GraphHeader.fact_epoch`, dense `NodeProvenanceStore`,
///   dense `EdgeProvenanceStore`, and `FileEntry` attribution fields (`content_hash`, `indexed_at`,
///   `source_uri`). The legacy `MAGIC_BYTES` / `VERSION` exports are preserved during Phase 1
///   to keep existing call sites compiling; later units bump the writer to V8 and treat V7 as
///   read-only.
pub const MAGIC_BYTES: &[u8; 13] = b"SQRY_GRAPH_V7";

/// Legacy V7 format version constant, preserved for existing call sites.
///
/// See [`CURRENT_VERSION`] / [`FormatVersion`] for the Phase 1+ versioning contract.
pub const VERSION: u32 = 7;

/// Phase 1 V7 magic bytes (re-export under the versioned name).
///
/// Equal to [`MAGIC_BYTES`]; the versioned name makes the legacy path explicit in
/// reader dispatch logic (`load_from_path` branching on magic bytes).
pub const MAGIC_BYTES_V7: &[u8; 13] = b"SQRY_GRAPH_V7";

/// Phase 1 V8 magic bytes.
///
/// Emitted by the Phase 1 fact-layer writer (P1U06) and accepted by the Phase 1
/// reader (P1U07). The magic is the sole versioning contract — no in-format
/// revision counter is introduced.
pub const MAGIC_BYTES_V8: &[u8; 13] = b"SQRY_GRAPH_V8";

/// Phase 2 V9 magic bytes.
///
/// Emitted by the Phase 2 binding-plane writer (P2U12) and accepted by the V9
/// reader. V9 extends V8 with `ScopeArena`, `AliasTable`, `ShadowTable`, and
/// `ScopeProvenanceStore` fields. V8 snapshots are upconverted to V9 inline on
/// load by running `derive_binding_plane`.
pub const MAGIC_BYTES_V9: &[u8; 13] = b"SQRY_GRAPH_V9";

/// Phase 3 V10 magic bytes.
///
/// Emitted by the Phase 3 derived-db writer (DB03) and accepted by the V10
/// reader. V10 extends V9 with `FileSegmentTable`. V9 snapshots are
/// upconverted to V10 inline on load by rebuilding the segment table from
/// the node arena.
pub const MAGIC_BYTES_V10: &[u8; 14] = b"SQRY_GRAPH_V10";

/// Phase A (C indirect-call precision) V11 magic bytes.
///
/// Emitted by the Phase A snapshot writer (U03) and accepted by the V11
/// reader. V11 extends V10 with:
/// - `StoredEntry { typed, flags }` metadata-store wire format (see U02).
///   The bitset-style `NodeFlags` channel lets a node carry SYNTHETIC,
///   `ADDRESS_TAKEN`, and `CALLSITE_PROMISCUOUS` independently of any
///   `TypedMetadata::Macro` or `TypedMetadata::Classpath` payload.
/// - Reserved `c_indirect_tables: Option<_>` slot on the snapshot envelope
///   for the Phase A C-icall side tables (populated by U09; absent in V10).
///
/// V10 snapshots are upconverted to V11 inline on load by mapping the legacy
/// metadata variants into the new `StoredEntry` shape and setting
/// `c_indirect_tables` to `None`.
pub const MAGIC_BYTES_V11: &[u8; 14] = b"SQRY_GRAPH_V11";

/// Phase β joint-stubs V12 magic bytes.
///
/// Emitted by the Phase β joint-stubs writer (this PR) and accepted by the
/// V12 reader. V12 extends V11 with TWO new snapshot-envelope slots that
/// carry per-snapshot side tables for the framework-routes (Plan A) and
/// dispatch-tables (Plan B) work. The shapes themselves are reserved-empty
/// in this PR; the downstream extractor / resolver PRs populate them.
///
/// V12 envelope additions:
/// - `framework_routes: BTreeMap<NodeId, FrameworkRouteMetadata>` — Plan A
///   slot. Empty for non-extractor workspaces (and for every V11 → V12
///   upconvert). Re-attached onto the in-memory
///   `NodeMetadataStore::framework_routes` field on load.
/// - `dispatch_tables: DispatchTables` — Plan B slot. Empty for
///   non-resolver workspaces (and for every V11 → V12 upconvert).
///   Re-attached onto the in-memory `NodeMetadataStore::dispatch_tables`
///   field on load.
///
/// V11 snapshots are upconverted to V12 inline on load by zero-initialising
/// the two new envelope slots. No metadata-store reshape is required (V11
/// metadata-store wire shape is preserved bit-for-bit inside V12 envelopes).
pub const MAGIC_BYTES_V12: &[u8; 14] = b"SQRY_GRAPH_V12";

/// T3 V13 magic bytes.
///
/// Emitted by the T3 writer (Cluster A) and accepted by the V13 reader. V13
/// is shape-identical to V12 — no fields are added or removed from the
/// snapshot data struct. The bump exists purely so V12 readers reject
/// snapshots containing the `EdgeKind::Wraps` variant (added in T3) rather
/// than silently failing to decode an unknown discriminant. V12 snapshots
/// upconvert to V13 inline on load via an identity-mapping `upconvert_v12_to_v13`.
pub const MAGIC_BYTES_V13: &[u8; 14] = b"SQRY_GRAPH_V13";

/// T2 V14 magic bytes.
///
/// Emitted by the T2 writer (Go channel pairing + generic instantiation) and
/// accepted by the V14 reader. V14 carries the new `NodeKind::Channel` variant
/// (inserted before `Other`, shifting `Other`'s positional postcard
/// discriminant) plus the additive `EdgeKind::ChannelPeer` /
/// `EdgeKind::Instantiates` variants. Because the `NodeKind` shift is NOT
/// additive, V13 snapshots are upconverted to V14 inline on load via
/// `upconvert_v13_to_v14`, which deserializes the node arena + `kind_index`
/// through the frozen `NodeKindV13` wire mirror (`legacy_v13.rs`) and
/// re-keys them under the new `NodeKind` layout. A V13-or-earlier reader
/// opening a V14 snapshot rejects it at the magic gate.
pub const MAGIC_BYTES_V14: &[u8; 14] = b"SQRY_GRAPH_V14";

/// Body-shape-descriptor V15 magic bytes.
///
/// Emitted by the V15 writer (per-function identifier-blind `ShapeDescriptor`
/// side table) and accepted by the V15 reader. V15 is shape-identical to V14 on
/// the wire **plus** one appended envelope slot: a flat
/// `Vec<(NodeId, ShapeDescriptor)>` carrying the `shape_descriptors` side table
/// (the `NodeMetadataStore` custom serde only emits `entries`, so the descriptors
/// ride a dedicated payload field, mirroring the V12 framework-route / dispatch
/// envelope slots). V14 snapshots upconvert to V15 inline on load via
/// `upconvert_v14_to_v15`, which moves every V14 field by value and leaves the
/// descriptor table empty. A V14-or-earlier reader opening a V15 snapshot rejects
/// it at the magic gate.
pub const MAGIC_BYTES_V15: &[u8; 14] = b"SQRY_GRAPH_V15";

/// Legacy V7 numeric version, exposed with a versioned name so the Phase 1 reader
/// dispatch can cite it explicitly. Equal to [`VERSION`].
pub const LEGACY_VERSION_V7: u32 = 7;

/// Typed snapshot format version.
///
/// Phase 1 introduces V8 as read/write and preserves V7 as a read-only compatibility
/// path. Phase 2 introduces V9 as read/write and preserves V8 as an upconvert path.
/// Later format additions bump the magic bytes (V10, …) rather than relying on any
/// in-format revision counter.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum FormatVersion {
    /// Legacy V7 — read-only after Phase 1 lands.
    V7 = 7,
    /// V8 — read/write after Phase 1, upconvert source after Phase 2.
    V8 = 8,
    /// V9 — read/write after Phase 2 (binding plane: `ScopeArena`, `AliasTable`,
    /// `ShadowTable`, `ScopeProvenanceStore`). V8 snapshots are upconverted to V9
    /// inline on load by running `derive_binding_plane`.
    V9 = 9,
    /// V10 — read/write after Phase 3 (derived DB: `FileSegmentTable`). V9
    /// snapshots are upconverted to V10 inline on load by rebuilding the
    /// segment table from the node arena.
    V10 = 10,
    /// V11 — read/write after Phase A C-icall precision (`StoredEntry`
    /// metadata wire format + reserved `c_indirect_tables` envelope slot).
    /// V10 snapshots are upconverted to V11 inline on load by mapping the
    /// legacy `NodeMetadata` variants into `StoredEntry { typed, flags }`
    /// and setting `c_indirect_tables` to `None`.
    V11 = 11,
    /// V12 — read/write after Phase β joint-stubs. Extends V11 with two new
    /// envelope slots: `framework_routes` (Plan A) and `dispatch_tables`
    /// (Plan B). Both are zero-initialised by the V11 → V12 upconvert.
    V12 = 12,
    /// V13 — read/write after T3 (error chains: `EdgeKind::Wraps`).
    /// Shape-identical to V12; the bump exists so V12 readers reject snapshots
    /// containing the new edge variant. V12 snapshots upconvert to V13 inline
    /// on load via an identity mapping.
    V13 = 13,
    /// V14 — read/write after T2 (Go channel pairing: `NodeKind::Channel`,
    /// `EdgeKind::ChannelPeer`; generic instantiation: `EdgeKind::Instantiates`).
    /// The `NodeKind::Channel` insert-before-`Other` shifts `Other`'s positional
    /// postcard discriminant, so V13 snapshots are upconverted to V14 inline on
    /// load via `upconvert_v13_to_v14`: the node arena + `kind_index` decode
    /// through the frozen `NodeKindV13` mirror, the arena is translated so
    /// legacy `Other` nodes land at their new position, and `kind_index` is
    /// rebuilt from the translated arena.
    V14 = 14,
    /// V15 — read/write after the body-shape-descriptor feature. Shape-identical
    /// to V14 on the wire plus one appended envelope slot carrying the
    /// `shape_descriptors` side table (`Vec<(NodeId, ShapeDescriptor)>`). V14
    /// snapshots upconvert to V15 inline on load via `upconvert_v14_to_v15`
    /// (every field moves by value, the descriptor table comes up empty), so an
    /// existing V14 snapshot loads with no descriptors until the next
    /// `sqry index --force` repopulates them. A V14-or-earlier reader rejects a
    /// V15 snapshot at the magic gate.
    V15 = 15,
}

impl FormatVersion {
    /// Returns the magic-byte sequence identifying this format version.
    #[must_use]
    pub const fn magic(self) -> &'static [u8] {
        match self {
            Self::V7 => MAGIC_BYTES_V7.as_slice(),
            Self::V8 => MAGIC_BYTES_V8.as_slice(),
            Self::V9 => MAGIC_BYTES_V9.as_slice(),
            Self::V10 => MAGIC_BYTES_V10.as_slice(),
            Self::V11 => MAGIC_BYTES_V11.as_slice(),
            Self::V12 => MAGIC_BYTES_V12.as_slice(),
            Self::V13 => MAGIC_BYTES_V13.as_slice(),
            Self::V14 => MAGIC_BYTES_V14.as_slice(),
            Self::V15 => MAGIC_BYTES_V15.as_slice(),
        }
    }

    /// Returns the numeric version tag (matches the trailing digit of the magic).
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// Parses a magic-byte prefix into a `FormatVersion`.
    ///
    /// Returns `None` if the bytes do not match any known format magic.
    #[must_use]
    pub fn from_magic(bytes: &[u8]) -> Option<Self> {
        // V15, V14, V13, V12, V11, and V10 magics are all 14 bytes. Check newest
        // first (V15 → V14 → … → V10) so a longer / newer magic is never
        // mis-classified as an older one by a less-strict comparison path.
        if bytes.len() >= MAGIC_BYTES_V15.len()
            && bytes[..MAGIC_BYTES_V15.len()] == *MAGIC_BYTES_V15
        {
            return Some(Self::V15);
        }
        if bytes.len() >= MAGIC_BYTES_V14.len()
            && bytes[..MAGIC_BYTES_V14.len()] == *MAGIC_BYTES_V14
        {
            return Some(Self::V14);
        }
        if bytes.len() >= MAGIC_BYTES_V13.len()
            && bytes[..MAGIC_BYTES_V13.len()] == *MAGIC_BYTES_V13
        {
            return Some(Self::V13);
        }
        if bytes.len() >= MAGIC_BYTES_V12.len()
            && bytes[..MAGIC_BYTES_V12.len()] == *MAGIC_BYTES_V12
        {
            return Some(Self::V12);
        }
        if bytes.len() >= MAGIC_BYTES_V11.len()
            && bytes[..MAGIC_BYTES_V11.len()] == *MAGIC_BYTES_V11
        {
            return Some(Self::V11);
        }
        if bytes.len() >= MAGIC_BYTES_V10.len()
            && bytes[..MAGIC_BYTES_V10.len()] == *MAGIC_BYTES_V10
        {
            return Some(Self::V10);
        }
        if bytes.len() < MAGIC_BYTES_V7.len() {
            return None;
        }
        let prefix = &bytes[..MAGIC_BYTES_V7.len()];
        if prefix == MAGIC_BYTES_V7 {
            Some(Self::V7)
        } else if prefix == MAGIC_BYTES_V8 {
            Some(Self::V8)
        } else if prefix == MAGIC_BYTES_V9 {
            Some(Self::V9)
        } else {
            None
        }
    }
}

/// Current writer format version (body-shape-descriptor side table: V15).
pub const CURRENT_VERSION: FormatVersion = FormatVersion::V15;

/// Header for persisted graph files.
///
/// The header provides metadata about the graph for validation
/// and efficient loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphHeader {
    /// Format version (for compatibility checking)
    pub version: u32,

    /// Number of nodes in the graph
    pub node_count: usize,

    /// Number of edges in the graph
    pub edge_count: usize,

    /// Number of interned strings
    pub string_count: usize,

    /// Number of registered files
    pub file_count: usize,

    /// Timestamp when graph was saved (unix epoch seconds)
    pub timestamp: u64,

    /// Configuration provenance - records which config was used to build this graph.
    #[serde(default)]
    pub config_provenance: Option<ConfigProvenance>,

    /// Plugin versions used to build this graph (`plugin_id` → version).
    ///
    /// Tracks which language plugin versions were active during indexing.
    /// Used to detect stale indexes when plugin versions change.
    #[serde(default)]
    pub plugin_versions: HashMap<String, String>,

    /// Monotonic fact-layer epoch stamped at save time (Phase 1+).
    ///
    /// Strictly increases across successive saves of the same snapshot file,
    /// including across process restarts: the writer reads the existing
    /// header (if any) before stamping and computes
    /// `max(prev_epoch + 1, SystemTime::now().as_secs())`.
    ///
    /// Defaulted to `0` for V7 snapshots and for `GraphHeader::new` /
    /// `with_provenance` constructors. The epoch is stamped by the Phase 1
    /// V8 writer (P1U06); this unit only introduces the field and accessors.
    ///
    /// Format: plain `u64`, serde-default `0` so postcard deserialization of
    /// older headers that did not carry the field continues to succeed.
    #[serde(default)]
    pub fact_epoch: u64,
}

impl GraphHeader {
    /// Creates a new graph header with the given counts.
    #[must_use]
    pub fn new(
        node_count: usize,
        edge_count: usize,
        string_count: usize,
        file_count: usize,
    ) -> Self {
        Self {
            version: VERSION,
            node_count,
            edge_count,
            string_count,
            file_count,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_provenance: None,
            plugin_versions: HashMap::new(),
            fact_epoch: 0,
        }
    }

    /// Creates a new graph header with config provenance.
    #[must_use]
    pub fn with_provenance(
        node_count: usize,
        edge_count: usize,
        string_count: usize,
        file_count: usize,
        provenance: ConfigProvenance,
    ) -> Self {
        Self {
            version: VERSION,
            node_count,
            edge_count,
            string_count,
            file_count,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_provenance: Some(provenance),
            plugin_versions: HashMap::new(),
            fact_epoch: 0,
        }
    }

    /// Creates a new graph header with config provenance and plugin versions.
    #[must_use]
    pub fn with_provenance_and_plugins(
        node_count: usize,
        edge_count: usize,
        string_count: usize,
        file_count: usize,
        provenance: ConfigProvenance,
        plugin_versions: HashMap<String, String>,
    ) -> Self {
        Self {
            version: VERSION,
            node_count,
            edge_count,
            string_count,
            file_count,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            config_provenance: Some(provenance),
            plugin_versions,
            fact_epoch: 0,
        }
    }

    /// Returns the config provenance if available.
    #[must_use]
    pub fn provenance(&self) -> Option<&ConfigProvenance> {
        self.config_provenance.as_ref()
    }

    /// Checks if the graph was built with tracked config provenance.
    #[must_use]
    pub fn has_provenance(&self) -> bool {
        self.config_provenance.is_some()
    }

    /// Returns the plugin versions used to build this graph.
    #[must_use]
    pub fn plugin_versions(&self) -> &HashMap<String, String> {
        &self.plugin_versions
    }

    /// Sets the plugin versions for this graph header.
    pub fn set_plugin_versions(&mut self, versions: HashMap<String, String>) {
        self.plugin_versions = versions;
    }

    /// Returns the monotonic fact-layer epoch stamped on this header.
    ///
    /// Returns `0` for headers created via `new` / `with_provenance` /
    /// `with_provenance_and_plugins` before the Phase 1 writer stamps a
    /// real epoch (P1U06), and for legacy V7 snapshots loaded through the
    /// backwards-read path (P1U07).
    #[must_use]
    pub fn fact_epoch(&self) -> u64 {
        self.fact_epoch
    }

    /// Sets the monotonic fact-layer epoch on this header.
    ///
    /// Intended for use by the Phase 1 V8 writer (P1U06), which computes
    /// the epoch via a `FactEpochClock` helper and stamps it immediately
    /// before serialization. Also used by tests.
    pub fn set_fact_epoch(&mut self, epoch: u64) {
        self.fact_epoch = epoch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_test_provenance() -> ConfigProvenance {
        ConfigProvenance {
            config_file: PathBuf::from(".sqry/graph/config/config.json"),
            config_checksum: "abc123def456".to_string(),
            schema_version: 1,
            overrides: HashMap::new(),
            build_timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            build_host: Some("test-host".to_string()),
        }
    }

    #[test]
    fn test_magic_bytes() {
        assert_eq!(MAGIC_BYTES, b"SQRY_GRAPH_V7");
        assert_eq!(MAGIC_BYTES.len(), 13);
    }

    #[test]
    fn test_version() {
        assert_eq!(VERSION, 7);
    }

    #[test]
    fn test_graph_header_new() {
        let header = GraphHeader::new(100, 50, 200, 10);

        assert_eq!(header.version, VERSION);
        assert_eq!(header.node_count, 100);
        assert_eq!(header.edge_count, 50);
        assert_eq!(header.string_count, 200);
        assert_eq!(header.file_count, 10);
        assert!(header.timestamp > 0);
        assert!(header.config_provenance.is_none());
    }

    #[test]
    fn test_graph_header_with_provenance() {
        let provenance = make_test_provenance();
        let header = GraphHeader::with_provenance(100, 50, 200, 10, provenance);

        assert_eq!(header.version, VERSION);
        assert_eq!(header.node_count, 100);
        assert_eq!(header.edge_count, 50);
        assert!(header.config_provenance.is_some());
        assert_eq!(
            header.config_provenance.as_ref().unwrap().config_checksum,
            "abc123def456"
        );
    }

    #[test]
    fn test_graph_header_provenance_method() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert!(header.provenance().is_none());

        let provenance = make_test_provenance();
        let header_with = GraphHeader::with_provenance(10, 5, 20, 2, provenance);
        assert!(header_with.provenance().is_some());
        assert_eq!(
            header_with.provenance().unwrap().config_checksum,
            "abc123def456"
        );
    }

    #[test]
    fn test_graph_header_has_provenance() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert!(!header.has_provenance());

        let provenance = make_test_provenance();
        let header_with = GraphHeader::with_provenance(10, 5, 20, 2, provenance);
        assert!(header_with.has_provenance());
    }

    #[test]
    fn test_graph_header_clone() {
        let header = GraphHeader::new(100, 50, 200, 10);
        let cloned = header.clone();

        assert_eq!(header.version, cloned.version);
        assert_eq!(header.node_count, cloned.node_count);
        assert_eq!(header.edge_count, cloned.edge_count);
        assert_eq!(header.string_count, cloned.string_count);
        assert_eq!(header.file_count, cloned.file_count);
    }

    #[test]
    fn test_graph_header_debug() {
        let header = GraphHeader::new(100, 50, 200, 10);
        let debug_str = format!("{header:?}");

        assert!(debug_str.contains("GraphHeader"));
        assert!(debug_str.contains("version"));
        assert!(debug_str.contains("node_count"));
    }

    #[test]
    fn test_graph_header_timestamp_is_recent() {
        let header = GraphHeader::new(10, 5, 20, 2);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // Timestamp should be within 1 second of now
        assert!(header.timestamp <= now);
        assert!(header.timestamp >= now - 1);
    }

    #[test]
    fn test_graph_header_zero_counts() {
        let header = GraphHeader::new(0, 0, 0, 0);

        assert_eq!(header.node_count, 0);
        assert_eq!(header.edge_count, 0);
        assert_eq!(header.string_count, 0);
        assert_eq!(header.file_count, 0);
    }

    #[test]
    fn test_graph_header_large_counts() {
        let header = GraphHeader::new(1_000_000, 5_000_000, 10_000_000, 100_000);

        assert_eq!(header.node_count, 1_000_000);
        assert_eq!(header.edge_count, 5_000_000);
        assert_eq!(header.string_count, 10_000_000);
        assert_eq!(header.file_count, 100_000);
    }

    #[test]
    fn test_graph_header_plugin_versions_empty_by_default() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert!(header.plugin_versions().is_empty());
    }

    #[test]
    fn test_graph_header_set_plugin_versions() {
        let mut header = GraphHeader::new(10, 5, 20, 2);

        let mut versions = HashMap::new();
        versions.insert("rust".to_string(), "3.3.0".to_string());
        versions.insert("javascript".to_string(), "3.3.0".to_string());

        header.set_plugin_versions(versions.clone());

        assert_eq!(header.plugin_versions().len(), 2);
        assert_eq!(
            header.plugin_versions().get("rust"),
            Some(&"3.3.0".to_string())
        );
        assert_eq!(
            header.plugin_versions().get("javascript"),
            Some(&"3.3.0".to_string())
        );
    }

    // ------------------------------------------------------------------
    // Phase 1 P1U02: GraphHeader.fact_epoch (additive u64)
    // ------------------------------------------------------------------

    #[test]
    fn phase1_graph_header_new_defaults_fact_epoch_to_zero() {
        let header = GraphHeader::new(10, 5, 20, 2);
        assert_eq!(header.fact_epoch, 0);
        assert_eq!(header.fact_epoch(), 0);
    }

    #[test]
    fn phase1_graph_header_with_provenance_defaults_fact_epoch_to_zero() {
        let header = GraphHeader::with_provenance(10, 5, 20, 2, make_test_provenance());
        assert_eq!(header.fact_epoch, 0);
    }

    #[test]
    fn phase1_graph_header_set_fact_epoch_round_trip() {
        let mut header = GraphHeader::new(10, 5, 20, 2);
        header.set_fact_epoch(42);
        assert_eq!(header.fact_epoch(), 42);
    }

    #[test]
    fn phase1_graph_header_postcard_round_trip_with_fact_epoch() {
        let mut header = GraphHeader::new(100, 50, 200, 10);
        header.set_fact_epoch(1_234_567);

        let encoded = postcard::to_allocvec(&header).expect("encode");
        let decoded: GraphHeader = postcard::from_bytes(&encoded).expect("decode");

        assert_eq!(decoded.fact_epoch(), 1_234_567);
        assert_eq!(decoded.node_count, 100);
        assert_eq!(decoded.edge_count, 50);
    }

    #[test]
    fn phase1_graph_header_fact_epoch_preserved_through_clone() {
        let mut header = GraphHeader::new(10, 5, 20, 2);
        header.set_fact_epoch(9_999);
        let cloned = header.clone();
        assert_eq!(cloned.fact_epoch(), 9_999);
    }

    // ------------------------------------------------------------------
    // Phase 1 P1U01: FormatVersion enum + V7/V8 magic constants
    // ------------------------------------------------------------------

    #[test]
    fn phase1_magic_bytes_v7_matches_legacy() {
        assert_eq!(MAGIC_BYTES_V7, b"SQRY_GRAPH_V7");
        assert_eq!(MAGIC_BYTES_V7, MAGIC_BYTES);
        assert_eq!(MAGIC_BYTES_V7.len(), 13);
    }

    #[test]
    fn phase1_magic_bytes_v8_is_distinct_and_13_bytes() {
        assert_eq!(MAGIC_BYTES_V8, b"SQRY_GRAPH_V8");
        assert_eq!(MAGIC_BYTES_V8.len(), 13);
        assert_ne!(MAGIC_BYTES_V8, MAGIC_BYTES_V7);
    }

    #[test]
    fn phase1_legacy_version_v7_equals_seven() {
        assert_eq!(LEGACY_VERSION_V7, 7);
    }

    #[test]
    fn phase1_format_version_discriminants() {
        assert_eq!(FormatVersion::V7 as u32, 7);
        assert_eq!(FormatVersion::V8 as u32, 8);
        assert_eq!(FormatVersion::V9 as u32, 9);
        assert_eq!(FormatVersion::V10 as u32, 10);
        assert_eq!(FormatVersion::V11 as u32, 11);
        assert_eq!(FormatVersion::V12 as u32, 12);
        assert_eq!(FormatVersion::V13 as u32, 13);
        assert_eq!(FormatVersion::V14 as u32, 14);
        assert_eq!(FormatVersion::V15 as u32, 15);
    }

    #[test]
    fn current_version_is_v15() {
        assert_eq!(CURRENT_VERSION, FormatVersion::V15);
    }

    #[test]
    fn shape_magic_bytes_v15_is_distinct_and_14_bytes() {
        assert_eq!(MAGIC_BYTES_V15, b"SQRY_GRAPH_V15");
        assert_eq!(MAGIC_BYTES_V15.len(), 14);
        assert_ne!(MAGIC_BYTES_V15.as_slice(), MAGIC_BYTES_V14.as_slice());
        assert_ne!(MAGIC_BYTES_V15.as_slice(), MAGIC_BYTES_V13.as_slice());
    }

    /// V15 must be tried before V14/older so a `SQRY_GRAPH_V15` prefix is never
    /// mis-classified as an older 14-byte magic.
    #[test]
    fn shape_format_version_dispatch_v15_before_older() {
        let mut buf = MAGIC_BYTES_V15.to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf), Some(FormatVersion::V15));

        let mut buf14 = MAGIC_BYTES_V14.to_vec();
        buf14.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf14), Some(FormatVersion::V14));
    }

    #[test]
    fn shape_format_version_v15_magic_round_trip() {
        let v = FormatVersion::V15;
        let bytes = v.magic();
        assert_eq!(bytes, MAGIC_BYTES_V15.as_slice());
        assert_eq!(FormatVersion::from_magic(bytes), Some(v));
    }

    #[test]
    fn t2_magic_bytes_v14_is_distinct_and_14_bytes() {
        assert_eq!(MAGIC_BYTES_V14, b"SQRY_GRAPH_V14");
        assert_eq!(MAGIC_BYTES_V14.len(), 14);
        assert_ne!(MAGIC_BYTES_V14.as_slice(), MAGIC_BYTES_V13.as_slice());
        assert_ne!(MAGIC_BYTES_V14.as_slice(), MAGIC_BYTES_V12.as_slice());
    }

    #[test]
    fn t2_format_version_from_magic_v14() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V14),
            Some(FormatVersion::V14),
        );
    }

    /// V14 must be tried before V13/V12/V11/V10 so a `SQRY_GRAPH_V14` prefix
    /// is never mis-classified as an older 14-byte magic.
    #[test]
    fn t2_format_version_dispatch_v14_before_older() {
        let mut buf = MAGIC_BYTES_V14.to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf), Some(FormatVersion::V14));

        let mut buf13 = MAGIC_BYTES_V13.to_vec();
        buf13.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf13), Some(FormatVersion::V13));
    }

    #[test]
    fn t2_format_version_v14_magic_round_trip() {
        let v = FormatVersion::V14;
        let bytes = v.magic();
        assert_eq!(bytes, MAGIC_BYTES_V14.as_slice());
        assert_eq!(FormatVersion::from_magic(bytes), Some(v));
    }

    #[test]
    fn t3_magic_bytes_v13_is_distinct_and_14_bytes() {
        assert_eq!(MAGIC_BYTES_V13, b"SQRY_GRAPH_V13");
        assert_eq!(MAGIC_BYTES_V13.len(), 14);
        assert_ne!(MAGIC_BYTES_V13.as_slice(), MAGIC_BYTES_V12.as_slice());
        assert_ne!(MAGIC_BYTES_V13.as_slice(), MAGIC_BYTES_V10.as_slice());
    }

    #[test]
    fn t3_format_version_from_magic_v13() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V13),
            Some(FormatVersion::V13),
        );
    }

    /// V13 and the other 14-byte magics (V12/V11/V10) must each resolve to
    /// their own version. The dispatch tries V13 FIRST, so a buffer starting
    /// with `SQRY_GRAPH_V13` must resolve to `FormatVersion::V13`. Guards
    /// against a future refactor that re-orders the comparisons and silently
    /// routes V13 through an older upconvert path.
    #[test]
    fn t3_format_version_dispatch_v13_before_v12_v11_v10() {
        let mut buf = MAGIC_BYTES_V13.to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf), Some(FormatVersion::V13));

        let mut buf12 = MAGIC_BYTES_V12.to_vec();
        buf12.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf12), Some(FormatVersion::V12));
    }

    #[test]
    fn t3_format_version_v13_magic_round_trip() {
        let v = FormatVersion::V13;
        let bytes = v.magic();
        assert_eq!(bytes, MAGIC_BYTES_V13.as_slice());
        assert_eq!(FormatVersion::from_magic(bytes), Some(v));
    }

    #[test]
    fn phase_a_magic_bytes_v11_is_distinct_and_14_bytes() {
        assert_eq!(MAGIC_BYTES_V11, b"SQRY_GRAPH_V11");
        assert_eq!(MAGIC_BYTES_V11.len(), 14);
        assert_ne!(MAGIC_BYTES_V11, MAGIC_BYTES_V10);
    }

    #[test]
    fn phase_a_format_version_from_magic_v11() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V11),
            Some(FormatVersion::V11),
        );
    }

    /// V11 and V10 magics are equal-length (14 bytes). The dispatch tries V11
    /// before V10, so a buffer starting with `SQRY_GRAPH_V11` must resolve to
    /// `FormatVersion::V11`, not V10. Guards against a future refactor that
    /// re-orders the comparisons and silently routes V11 through the V10
    /// upconvert path.
    #[test]
    fn phase_a_format_version_dispatch_v11_before_v10() {
        let mut buf = MAGIC_BYTES_V11.to_vec();
        // Append trailing bytes so the dispatch sees a "real-looking" payload.
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf), Some(FormatVersion::V11));

        let mut buf10 = MAGIC_BYTES_V10.to_vec();
        buf10.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf10), Some(FormatVersion::V10));
    }

    #[test]
    fn phase_a_format_version_v11_magic_round_trip() {
        let v = FormatVersion::V11;
        let bytes = v.magic();
        assert_eq!(bytes, MAGIC_BYTES_V11.as_slice());
        assert_eq!(FormatVersion::from_magic(bytes), Some(v));
    }

    #[test]
    fn phase1_format_version_from_magic_v7() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V7),
            Some(FormatVersion::V7),
        );
    }

    #[test]
    fn phase1_format_version_from_magic_v8() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V8),
            Some(FormatVersion::V8),
        );
    }

    #[test]
    fn phase2_magic_bytes_v9_is_distinct_and_13_bytes() {
        assert_eq!(MAGIC_BYTES_V9, b"SQRY_GRAPH_V9");
        assert_eq!(MAGIC_BYTES_V9.len(), 13);
        assert_ne!(MAGIC_BYTES_V9, MAGIC_BYTES_V7);
        assert_ne!(MAGIC_BYTES_V9, MAGIC_BYTES_V8);
    }

    #[test]
    fn phase2_format_version_from_magic_v9() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V9),
            Some(FormatVersion::V9),
        );
    }

    #[test]
    fn phase1_format_version_from_magic_unknown() {
        assert_eq!(FormatVersion::from_magic(b"SQRY_GRAPH_V1"), None);
        assert_eq!(FormatVersion::from_magic(b"NOT_A_GRAPH_!"), None);
    }

    #[test]
    fn phase1_format_version_magic_round_trip() {
        for version in [
            FormatVersion::V7,
            FormatVersion::V8,
            FormatVersion::V9,
            FormatVersion::V10,
            FormatVersion::V11,
            FormatVersion::V12,
            FormatVersion::V13,
        ] {
            let bytes = version.magic();
            assert_eq!(FormatVersion::from_magic(bytes), Some(version));
        }
    }

    // ------------------------------------------------------------------
    // Phase β joint-stubs: V12 magic + dispatch
    // ------------------------------------------------------------------

    #[test]
    fn phase_beta_magic_bytes_v12_is_distinct_and_14_bytes() {
        assert_eq!(MAGIC_BYTES_V12, b"SQRY_GRAPH_V12");
        assert_eq!(MAGIC_BYTES_V12.len(), 14);
        assert_ne!(MAGIC_BYTES_V12, MAGIC_BYTES_V11);
        assert_ne!(MAGIC_BYTES_V12, MAGIC_BYTES_V10);
    }

    #[test]
    fn phase_beta_format_version_from_magic_v12() {
        assert_eq!(
            FormatVersion::from_magic(MAGIC_BYTES_V12),
            Some(FormatVersion::V12),
        );
    }

    /// V12, V11, and V10 magics are equal-length (14 bytes). The dispatch
    /// tries V12 FIRST, then V11, then V10 — so a buffer that starts with
    /// `SQRY_GRAPH_V12` must resolve to V12, not V11 or V10. Mirrors the
    /// `phase_a_format_version_dispatch_v11_before_v10` guard.
    #[test]
    fn phase_beta_format_version_dispatch_v12_before_v11_v10() {
        let mut buf = MAGIC_BYTES_V12.to_vec();
        buf.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf), Some(FormatVersion::V12));

        let mut buf11 = MAGIC_BYTES_V11.to_vec();
        buf11.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf11), Some(FormatVersion::V11));

        let mut buf10 = MAGIC_BYTES_V10.to_vec();
        buf10.extend_from_slice(&[0u8; 8]);
        assert_eq!(FormatVersion::from_magic(&buf10), Some(FormatVersion::V10));
    }

    #[test]
    fn phase_beta_format_version_v12_magic_round_trip() {
        let v = FormatVersion::V12;
        let bytes = v.magic();
        assert_eq!(bytes, MAGIC_BYTES_V12.as_slice());
        assert_eq!(FormatVersion::from_magic(bytes), Some(v));
    }

    #[test]
    fn phase1_format_version_copy_eq_debug() {
        let v = FormatVersion::V8;
        let copied = v;
        assert_eq!(v, copied);
        assert_eq!(format!("{v:?}"), "V8");
    }

    #[test]
    fn phase2_format_version_v9_copy_eq_debug() {
        let v = FormatVersion::V9;
        let copied = v;
        assert_eq!(v, copied);
        assert_eq!(format!("{v:?}"), "V9");
    }

    #[test]
    fn test_graph_header_with_provenance_and_plugins() {
        let provenance = make_test_provenance();

        let mut plugin_versions = HashMap::new();
        plugin_versions.insert("rust".to_string(), "3.3.0".to_string());
        plugin_versions.insert("python".to_string(), "3.3.0".to_string());

        let header = GraphHeader::with_provenance_and_plugins(
            100,
            50,
            200,
            10,
            provenance,
            plugin_versions.clone(),
        );

        assert_eq!(header.version, VERSION);
        assert_eq!(header.node_count, 100);
        assert!(header.config_provenance.is_some());
        assert_eq!(header.plugin_versions().len(), 2);
        assert_eq!(
            header.plugin_versions().get("rust"),
            Some(&"3.3.0".to_string())
        );
    }
}
