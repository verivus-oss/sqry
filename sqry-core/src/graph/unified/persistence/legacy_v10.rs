//! Versioned V10 wire types for the snapshot reader.
//!
//! # Why this module exists
//!
//! V10 (`SQRY_GRAPH_V10`) is a closed wire format. New live-type fields
//! added in U04+ (`EdgeKind::Calls.resolved_via`, future
//! `NodeMetadata`-side flags, etc.) MUST NOT shift the V10 postcard wire
//! layout. The V10 reader path therefore deserializes into **versioned V10
//! mirror types** defined here — not into the live `EdgeKind` /
//! `BidirectionalEdgeStore` / `NodeMetadataStore` directly — and then
//! translates to the live (V11) shapes via
//! [`super::snapshot::upconvert_v10_to_v11`].
//!
//! The codex iter-1 review of U03 (BLOCKERs at
//! `sqry-core/src/graph/unified/persistence/snapshot.rs:1949` and
//! `sqry-core/src/graph/unified/storage/metadata.rs:326`) flagged that the
//! prior implementation deserialized V10 bytes directly into the live
//! `BidirectionalEdgeStore` / `EdgeKind` / `NodeMetadataStore`, which would
//! break the moment any of those live types changed. This module locks the
//! V10 wire format to pre-U04 / pre-U02 shapes regardless of how the live
//! types evolve.
//!
//! # Scope
//!
//! Wire types here are intentionally **deserialize-driven** from postcard
//! bytes; we do not write fresh V10 snapshots in production (writers
//! always stamp the current `MAGIC_BYTES_V11`). The mirror types still
//! derive `Serialize` so the U03 V10-payload regression tests in
//! `snapshot.rs` can hand-craft a V10 frame through `write_framed_v10`.
//!
//! # What's mirrored
//!
//! * [`EdgeKindV10`] — every variant of the pre-U04 live `EdgeKind`, with
//!   `Calls { argument_count, is_async }` (no `resolved_via`).
//! * [`DeltaEdgeV10`], [`DeltaBufferV10`], [`CsrGraphV10`], [`EdgeStoreV10`],
//!   [`BidirectionalEdgeStoreV10`] — every wire-bearing type that nests
//!   `EdgeKind` inside its serialized payload.
//! * [`NodeMetadataV10`] + [`NodeMetadataEntryV10`] + [`NodeMetadataStoreV10`]
//!   — the pre-U02 metadata store: three `NodeMetadata` variants
//!   (`Macro` / `Classpath` / `Synthetic`) with no `flags` byte.
//!
//! Other components in `GraphSnapshotDataV10` (`NodeArena`,
//! `StringInterner`, `FileRegistry`, `AuxiliaryIndices`, scope/alias/shadow
//! tables, `FileSegmentTable`, `NodeProvenanceStore`, `EdgeProvenanceStore`)
//! do not nest `EdgeKind` or pre-U02 metadata and are therefore reused
//! directly from the live types — additions to those types in later phases
//! would still require their own versioning if they ever touch the wire
//! shape, but as of U03 they are wire-stable.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::graph::node::Span;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::edge::delta::DeltaOp;
use crate::graph::unified::edge::kind::{
    DbQueryType, ExportKind, FfiConvention, HttpMethod, LifetimeConstraintKind, MacroExpansionKind,
    MqProtocol, ResolvedVia, TableWriteOp, TypeOfContext,
};
use crate::graph::unified::file::id::FileId;
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::storage::{
    ClasspathNodeMetadata, MacroNodeMetadata, NodeFlags, NodeMetadataStore, StoredEntry,
    TypedMetadata,
};
use crate::graph::unified::string::StringId;

// ============================================================================
// EdgeKindV10 — pre-U04 wire type, mirrors live EdgeKind variant-for-variant.
// ============================================================================

/// Pre-U04 wire-format mirror of `EdgeKind`.
///
/// Every variant matches the live `EdgeKind` byte-for-byte under postcard
/// encoding. Critically, `Calls { argument_count, is_async }` carries
/// **only** the two pre-U04 fields — U04 adds `resolved_via` to the live
/// type, but V10 snapshots never wrote that field.
///
/// `#[serde(rename_all = "snake_case")]` matches the live attribute so the
/// JSON wire form (when used) is identical.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EdgeKindV10 {
    Defines,
    Contains,
    Calls {
        argument_count: u8,
        is_async: bool,
    },
    References,
    Imports {
        alias: Option<StringId>,
        is_wildcard: bool,
    },
    Exports {
        kind: ExportKind,
        alias: Option<StringId>,
    },
    TypeOf {
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<StringId>,
    },
    Inherits,
    Implements,
    LifetimeConstraint {
        constraint_kind: LifetimeConstraintKind,
    },
    TraitMethodBinding {
        trait_name: StringId,
        impl_type: StringId,
        is_ambiguous: bool,
    },
    MacroExpansion {
        expansion_kind: MacroExpansionKind,
        is_verified: bool,
    },
    FfiCall {
        convention: FfiConvention,
    },
    HttpRequest {
        method: HttpMethod,
        url: Option<StringId>,
    },
    GrpcCall {
        service: StringId,
        method: StringId,
    },
    WebAssemblyCall,
    DbQuery {
        query_type: DbQueryType,
        table: Option<StringId>,
    },
    TableRead {
        table_name: StringId,
        schema: Option<StringId>,
    },
    TableWrite {
        table_name: StringId,
        schema: Option<StringId>,
        operation: TableWriteOp,
    },
    TriggeredBy {
        trigger_name: StringId,
        schema: Option<StringId>,
    },
    MessageQueue {
        protocol: MqProtocol,
        topic: Option<StringId>,
    },
    WebSocket {
        event: Option<StringId>,
    },
    GraphQLOperation {
        operation: StringId,
    },
    ProcessExec {
        command: StringId,
    },
    FileIpc {
        path_pattern: Option<StringId>,
    },
    ProtocolCall {
        protocol: StringId,
        metadata: Option<StringId>,
    },
    GenericBound,
    AnnotatedWith,
    AnnotationParam,
    LambdaCaptures,
    ModuleExports,
    ModuleRequires,
    ModuleOpens,
    ModuleProvides,
    TypeArgument,
    ExtensionReceiver,
    CompanionOf,
    SealedPermit,
}

/// Translate a V10 [`EdgeKindV10`] to the live (V11) `EdgeKind`.
///
/// # U04 integration anchor
///
/// When `U04_C_ICALL_PRECISION` lands on this feat branch it adds the
/// `resolved_via: ResolvedVia` field to `EdgeKind::Calls`. The `Calls` arm
/// below will then fail to compile (missing field), which is the
/// **deliberate safety net** for the V10 → V11 wire translation. The
/// one-line fix is documented in the inline TODO comment below: stamp
/// `resolved_via: ResolvedVia::Direct` for every V10 `Calls` edge (V10
/// snapshots predate the indirect-call resolver, so by construction every
/// `Calls` edge they carry resolved to a single definition — `Direct`).
pub(crate) fn translate_edge_v10_to_v11(v10: EdgeKindV10) -> EdgeKind {
    match v10 {
        EdgeKindV10::Defines => EdgeKind::Defines,
        EdgeKindV10::Contains => EdgeKind::Contains,
        // U04 integration: V10 snapshots predate the indirect-call resolver,
        // so every `Calls` edge in a V10 snapshot is by construction a
        // directly resolved single-definition call. Stamp `ResolvedVia::Direct`
        // on the V11 output.
        EdgeKindV10::Calls {
            argument_count,
            is_async,
        } => EdgeKind::Calls {
            argument_count,
            is_async,
            resolved_via: ResolvedVia::Direct,
        },
        EdgeKindV10::References => EdgeKind::References,
        EdgeKindV10::Imports { alias, is_wildcard } => EdgeKind::Imports { alias, is_wildcard },
        EdgeKindV10::Exports { kind, alias } => EdgeKind::Exports { kind, alias },
        EdgeKindV10::TypeOf {
            context,
            index,
            name,
        } => EdgeKind::TypeOf {
            context,
            index,
            name,
        },
        EdgeKindV10::Inherits => EdgeKind::Inherits,
        EdgeKindV10::Implements => EdgeKind::Implements,
        EdgeKindV10::LifetimeConstraint { constraint_kind } => {
            EdgeKind::LifetimeConstraint { constraint_kind }
        }
        EdgeKindV10::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        },
        EdgeKindV10::MacroExpansion {
            expansion_kind,
            is_verified,
        } => EdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        },
        EdgeKindV10::FfiCall { convention } => EdgeKind::FfiCall { convention },
        EdgeKindV10::HttpRequest { method, url } => EdgeKind::HttpRequest { method, url },
        EdgeKindV10::GrpcCall { service, method } => EdgeKind::GrpcCall { service, method },
        EdgeKindV10::WebAssemblyCall => EdgeKind::WebAssemblyCall,
        EdgeKindV10::DbQuery { query_type, table } => EdgeKind::DbQuery { query_type, table },
        EdgeKindV10::TableRead { table_name, schema } => EdgeKind::TableRead { table_name, schema },
        EdgeKindV10::TableWrite {
            table_name,
            schema,
            operation,
        } => EdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        },
        EdgeKindV10::TriggeredBy {
            trigger_name,
            schema,
        } => EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        },
        EdgeKindV10::MessageQueue { protocol, topic } => EdgeKind::MessageQueue { protocol, topic },
        EdgeKindV10::WebSocket { event } => EdgeKind::WebSocket { event },
        EdgeKindV10::GraphQLOperation { operation } => EdgeKind::GraphQLOperation { operation },
        EdgeKindV10::ProcessExec { command } => EdgeKind::ProcessExec { command },
        EdgeKindV10::FileIpc { path_pattern } => EdgeKind::FileIpc { path_pattern },
        EdgeKindV10::ProtocolCall { protocol, metadata } => {
            EdgeKind::ProtocolCall { protocol, metadata }
        }
        EdgeKindV10::GenericBound => EdgeKind::GenericBound,
        EdgeKindV10::AnnotatedWith => EdgeKind::AnnotatedWith,
        EdgeKindV10::AnnotationParam => EdgeKind::AnnotationParam,
        EdgeKindV10::LambdaCaptures => EdgeKind::LambdaCaptures,
        EdgeKindV10::ModuleExports => EdgeKind::ModuleExports,
        EdgeKindV10::ModuleRequires => EdgeKind::ModuleRequires,
        EdgeKindV10::ModuleOpens => EdgeKind::ModuleOpens,
        EdgeKindV10::ModuleProvides => EdgeKind::ModuleProvides,
        EdgeKindV10::TypeArgument => EdgeKind::TypeArgument,
        EdgeKindV10::ExtensionReceiver => EdgeKind::ExtensionReceiver,
        EdgeKindV10::CompanionOf => EdgeKind::CompanionOf,
        EdgeKindV10::SealedPermit => EdgeKind::SealedPermit,
    }
}

/// Reverse direction (V11 → V10), used by U03 tests that hand-craft a V10
/// payload via `write_framed_v10`. Total as of U03; once U04 adds
/// `resolved_via`, the `Calls` arm will need a one-line update to drop the
/// new field (V10 has no slot for it).
pub(crate) fn translate_edge_v11_to_v10(v11: EdgeKind) -> EdgeKindV10 {
    match v11 {
        EdgeKind::Defines => EdgeKindV10::Defines,
        EdgeKind::Contains => EdgeKindV10::Contains,
        // U04 integration: V10 has no slot for `resolved_via`. Destructure
        // and discard it on the V11 → V10 hand-off. The U03 tests that drive
        // this path only build `Direct`-resolved edges, so no information is
        // lost; the wire shape is what V10 readers expect.
        EdgeKind::Calls {
            argument_count,
            is_async,
            resolved_via: _,
        } => EdgeKindV10::Calls {
            argument_count,
            is_async,
        },
        EdgeKind::References => EdgeKindV10::References,
        EdgeKind::Imports { alias, is_wildcard } => EdgeKindV10::Imports { alias, is_wildcard },
        EdgeKind::Exports { kind, alias } => EdgeKindV10::Exports { kind, alias },
        EdgeKind::TypeOf {
            context,
            index,
            name,
        } => EdgeKindV10::TypeOf {
            context,
            index,
            name,
        },
        EdgeKind::Inherits => EdgeKindV10::Inherits,
        EdgeKind::Implements => EdgeKindV10::Implements,
        EdgeKind::LifetimeConstraint { constraint_kind } => {
            EdgeKindV10::LifetimeConstraint { constraint_kind }
        }
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => EdgeKindV10::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        },
        EdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        } => EdgeKindV10::MacroExpansion {
            expansion_kind,
            is_verified,
        },
        EdgeKind::FfiCall { convention } => EdgeKindV10::FfiCall { convention },
        EdgeKind::HttpRequest { method, url } => EdgeKindV10::HttpRequest { method, url },
        EdgeKind::GrpcCall { service, method } => EdgeKindV10::GrpcCall { service, method },
        EdgeKind::WebAssemblyCall => EdgeKindV10::WebAssemblyCall,
        EdgeKind::DbQuery { query_type, table } => EdgeKindV10::DbQuery { query_type, table },
        EdgeKind::TableRead { table_name, schema } => EdgeKindV10::TableRead { table_name, schema },
        EdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        } => EdgeKindV10::TableWrite {
            table_name,
            schema,
            operation,
        },
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => EdgeKindV10::TriggeredBy {
            trigger_name,
            schema,
        },
        EdgeKind::MessageQueue { protocol, topic } => EdgeKindV10::MessageQueue { protocol, topic },
        EdgeKind::WebSocket { event } => EdgeKindV10::WebSocket { event },
        EdgeKind::GraphQLOperation { operation } => EdgeKindV10::GraphQLOperation { operation },
        EdgeKind::ProcessExec { command } => EdgeKindV10::ProcessExec { command },
        EdgeKind::FileIpc { path_pattern } => EdgeKindV10::FileIpc { path_pattern },
        EdgeKind::ProtocolCall { protocol, metadata } => {
            EdgeKindV10::ProtocolCall { protocol, metadata }
        }
        EdgeKind::GenericBound => EdgeKindV10::GenericBound,
        EdgeKind::AnnotatedWith => EdgeKindV10::AnnotatedWith,
        EdgeKind::AnnotationParam => EdgeKindV10::AnnotationParam,
        EdgeKind::LambdaCaptures => EdgeKindV10::LambdaCaptures,
        EdgeKind::ModuleExports => EdgeKindV10::ModuleExports,
        EdgeKind::ModuleRequires => EdgeKindV10::ModuleRequires,
        EdgeKind::ModuleOpens => EdgeKindV10::ModuleOpens,
        EdgeKind::ModuleProvides => EdgeKindV10::ModuleProvides,
        EdgeKind::TypeArgument => EdgeKindV10::TypeArgument,
        EdgeKind::ExtensionReceiver => EdgeKindV10::ExtensionReceiver,
        EdgeKind::CompanionOf => EdgeKindV10::CompanionOf,
        EdgeKind::SealedPermit => EdgeKindV10::SealedPermit,
    }
}

// ============================================================================
// DeltaEdgeV10 / DeltaBufferV10 — wire-stable mirrors of the live
// `DeltaEdge` / `DeltaBuffer` types, using `EdgeKindV10` instead of `EdgeKind`.
// ============================================================================

/// Pre-U04 wire-format mirror of `DeltaEdge`. Field order must match the
/// live struct's serialization layout exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DeltaEdgeV10 {
    pub(crate) source: NodeId,
    pub(crate) target: NodeId,
    pub(crate) kind: EdgeKindV10,
    pub(crate) seq: u64,
    pub(crate) op: DeltaOp,
    pub(crate) file: FileId,
    pub(crate) spans: Vec<Span>,
}

/// Pre-U04 wire-format mirror of `DeltaBuffer`. Field order must match the
/// live struct exactly so postcard sees the same on-wire schema.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DeltaBufferV10 {
    pub(crate) edges: HashMap<FileId, Vec<DeltaEdgeV10>>,
    pub(crate) edge_count: usize,
    pub(crate) byte_size: usize,
    #[serde(with = "atomic_u64_serde_v10")]
    pub(crate) seq_counter: AtomicU64,
}

/// `AtomicU64` serde shim (mirrors the private `atomic_u64_serde` in
/// `edge/delta.rs`). Local copy avoids cross-module visibility leak.
pub(crate) mod atomic_u64_serde_v10 {
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(value: &AtomicU64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.load(Ordering::SeqCst).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<AtomicU64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Ok(AtomicU64::new(value))
    }
}

// ============================================================================
// CsrGraphV10 / EdgeStoreV10 / BidirectionalEdgeStoreV10
// ============================================================================

/// Pre-U04 wire-format mirror of `CsrGraph`. Field order matches the live
/// struct exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CsrGraphV10 {
    pub(crate) node_count: usize,
    pub(crate) row_ptr: Vec<u32>,
    pub(crate) col_idx: Vec<NodeId>,
    pub(crate) edge_kind: Vec<EdgeKindV10>,
    pub(crate) edge_seq: Vec<u64>,
    pub(crate) edge_spans: Vec<Vec<Span>>,
}

/// Pre-U04 wire-format mirror of `EdgeStore`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct EdgeStoreV10 {
    pub(crate) csr: Option<CsrGraphV10>,
    pub(crate) csr_tombstones: Vec<bool>,
    pub(crate) csr_version: u64,
    pub(crate) delta: DeltaBufferV10,
}

/// Pre-U04 wire-format mirror of `BidirectionalEdgeStore`. Uses
/// [`rwlock_edge_store_serde_v10`] to (de)serialize the inner
/// `EdgeStoreV10` as a flat struct (matching the live store's wrapping).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct BidirectionalEdgeStoreV10 {
    #[serde(with = "rwlock_edge_store_serde_v10")]
    pub(crate) forward: RwLock<EdgeStoreV10>,
    #[serde(with = "rwlock_edge_store_serde_v10")]
    pub(crate) reverse: RwLock<EdgeStoreV10>,
}

/// `RwLock<EdgeStoreV10>` serde shim (mirrors the private
/// `rwlock_edge_store_serde` in `edge/bidirectional.rs`).
pub(crate) mod rwlock_edge_store_serde_v10 {
    use parking_lot::RwLock;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::EdgeStoreV10;

    pub fn serialize<S>(value: &RwLock<EdgeStoreV10>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.read().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<RwLock<EdgeStoreV10>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let store = EdgeStoreV10::deserialize(deserializer)?;
        Ok(RwLock::new(store))
    }
}

// ============================================================================
// V10 → live edge-store translations
// ============================================================================

pub(crate) fn translate_delta_edge_v10_to_v11(
    v10: DeltaEdgeV10,
) -> crate::graph::unified::edge::delta::DeltaEdge {
    use crate::graph::unified::edge::delta::DeltaEdge;
    DeltaEdge {
        source: v10.source,
        target: v10.target,
        kind: translate_edge_v10_to_v11(v10.kind),
        seq: v10.seq,
        op: v10.op,
        file: v10.file,
        spans: v10.spans,
    }
}

pub(crate) fn translate_delta_buffer_v10_to_v11(
    v10: DeltaBufferV10,
) -> crate::graph::unified::edge::delta::DeltaBuffer {
    use std::sync::atomic::Ordering;

    use crate::graph::unified::edge::delta::DeltaBuffer;
    let mut buffer = DeltaBuffer::new();
    let DeltaBufferV10 {
        edges,
        edge_count: _,
        byte_size: _,
        seq_counter,
    } = v10;
    // Push every edge; `push` re-derives `edge_count` + `byte_size`.
    for (_file, edge_vec) in edges {
        for edge in edge_vec {
            buffer.push(translate_delta_edge_v10_to_v11(edge));
        }
    }
    // Restore the persisted high-water mark so subsequent `next_seq()`
    // allocations continue from there (live `push` does not touch the
    // counter — it is allocated by `next_seq()` ahead of `push`).
    buffer.advance_seq_to(seq_counter.load(Ordering::SeqCst));
    buffer
}

pub(crate) fn translate_csr_graph_v10_to_v11(
    v10: CsrGraphV10,
) -> crate::graph::unified::storage::csr::CsrGraph {
    use crate::graph::unified::storage::csr::CsrGraph;
    let edge_kind_v11: Vec<EdgeKind> = v10
        .edge_kind
        .into_iter()
        .map(translate_edge_v10_to_v11)
        .collect();
    CsrGraph::from_raw(
        v10.node_count,
        v10.row_ptr,
        v10.col_idx,
        edge_kind_v11,
        v10.edge_seq,
        v10.edge_spans,
    )
}

pub(crate) fn translate_edge_store_v10_to_v11(
    v10: EdgeStoreV10,
) -> crate::graph::unified::edge::store::EdgeStore {
    use crate::graph::unified::edge::store::EdgeStore;
    let csr = v10.csr.map(translate_csr_graph_v10_to_v11);
    let delta = translate_delta_buffer_v10_to_v11(v10.delta);
    EdgeStore::from_parts_v10_upconvert(csr, v10.csr_tombstones, v10.csr_version, delta)
}

pub(crate) fn translate_bidirectional_edge_store_v10_to_v11(
    v10: BidirectionalEdgeStoreV10,
) -> crate::graph::unified::BidirectionalEdgeStore {
    use crate::graph::unified::BidirectionalEdgeStore;
    let forward = translate_edge_store_v10_to_v11(v10.forward.into_inner());
    let reverse = translate_edge_store_v10_to_v11(v10.reverse.into_inner());
    BidirectionalEdgeStore::from_parts_v10_upconvert(forward, reverse)
}

// ============================================================================
// V11 → V10 edge-store translations (test-only writer path)
// ============================================================================

pub(crate) fn translate_delta_edge_v11_to_v10(
    v11: crate::graph::unified::edge::delta::DeltaEdge,
) -> DeltaEdgeV10 {
    let crate::graph::unified::edge::delta::DeltaEdge {
        source,
        target,
        kind,
        seq,
        op,
        file,
        spans,
    } = v11;
    DeltaEdgeV10 {
        source,
        target,
        kind: translate_edge_v11_to_v10(kind),
        seq,
        op,
        file,
        spans,
    }
}

pub(crate) fn translate_delta_buffer_v11_to_v10(
    v11: &crate::graph::unified::edge::delta::DeltaBuffer,
) -> DeltaBufferV10 {
    let mut edges: HashMap<FileId, Vec<DeltaEdgeV10>> = HashMap::new();
    let mut edge_count: usize = 0;
    let mut byte_size: usize = 0;
    for edge in v11.iter() {
        let v10_edge = translate_delta_edge_v11_to_v10(edge.clone());
        byte_size += edge.byte_size();
        edge_count += 1;
        edges.entry(v10_edge.file).or_default().push(v10_edge);
    }
    DeltaBufferV10 {
        edges,
        edge_count,
        byte_size,
        seq_counter: AtomicU64::new(v11.current_seq()),
    }
}

pub(crate) fn translate_csr_graph_v11_to_v10(
    v11: &crate::graph::unified::storage::csr::CsrGraph,
) -> CsrGraphV10 {
    let edge_kind_v10: Vec<EdgeKindV10> = v11
        .edge_kind_slice()
        .iter()
        .cloned()
        .map(translate_edge_v11_to_v10)
        .collect();
    CsrGraphV10 {
        node_count: v11.node_count(),
        row_ptr: v11.row_ptr_slice().to_vec(),
        col_idx: v11.col_idx_slice().to_vec(),
        edge_kind: edge_kind_v10,
        edge_seq: v11.edge_seq_slice().to_vec(),
        edge_spans: v11.edge_spans_slice().to_vec(),
    }
}

pub(crate) fn translate_edge_store_v11_to_v10(
    v11: &crate::graph::unified::edge::store::EdgeStore,
) -> EdgeStoreV10 {
    let csr = v11.csr().map(translate_csr_graph_v11_to_v10);
    let csr_tombstones = v11.csr_tombstones_slice().to_vec();
    let csr_version = v11.csr_version();
    let delta = translate_delta_buffer_v11_to_v10(v11.delta());
    EdgeStoreV10 {
        csr,
        csr_tombstones,
        csr_version,
        delta,
    }
}

pub(crate) fn translate_bidirectional_edge_store_v11_to_v10(
    v11: &crate::graph::unified::BidirectionalEdgeStore,
) -> BidirectionalEdgeStoreV10 {
    let forward = translate_edge_store_v11_to_v10(&v11.forward());
    let reverse = translate_edge_store_v11_to_v10(&v11.reverse());
    BidirectionalEdgeStoreV10 {
        forward: RwLock::new(forward),
        reverse: RwLock::new(reverse),
    }
}

// ============================================================================
// NodeMetadataV10 — pre-U02 three-variant `NodeMetadata`, no flags byte.
// ============================================================================

/// Pre-U02 wire-format `NodeMetadata` enum.
///
/// Mirrors the master-tip (`02799e8c5:sqry-core/src/graph/unified/storage/metadata.rs`)
/// definition exactly: three variants, `#[serde(tag = "kind", rename_all =
/// "snake_case")]` — `Macro` carries a `MacroNodeMetadata`, `Classpath`
/// carries a `ClasspathNodeMetadata`, `Synthetic` is a unit variant.
///
/// In U03's V10 → V11 upconvert, each variant maps to a [`StoredEntry`]:
///
/// * `Macro(m)`     → `StoredEntry { typed: Some(TypedMetadata::Macro(m)), flags: NodeFlags::EMPTY }`
/// * `Classpath(c)` → `StoredEntry { typed: Some(TypedMetadata::Classpath(c)), flags: NodeFlags::EMPTY }`
/// * `Synthetic`    → `StoredEntry { typed: None, flags: NodeFlags::SYNTHETIC }`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[allow(dead_code)] // payload enum kept for documentation / future direct serde wiring
pub(crate) enum NodeMetadataV10 {
    Macro(MacroNodeMetadata),
    Classpath(ClasspathNodeMetadata),
    Synthetic,
}

/// Pre-U02 wire-format entry for a single metadata record.
///
/// Matches `02799e8c5:sqry-core/src/graph/unified/storage/metadata.rs`
/// V7-shaped layout: `index`, `generation`, `kind` discriminant byte, plus
/// the two payload `Option`s. There is **no** `flags: u8` byte in the V10
/// wire layout — this is the precise shape that breaks if U02's
/// `NodeMetadataEntryV11` deserializer is applied to V10 bytes (codex
/// iter-1 BLOCKER at `metadata.rs:326`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct NodeMetadataEntryV10 {
    pub(crate) index: u32,
    pub(crate) generation: u64,
    /// Legacy V7 discriminant: 0 = Macro, 1 = Classpath, 2 = Synthetic.
    pub(crate) kind: u8,
    pub(crate) macro_data: Option<MacroNodeMetadata>,
    pub(crate) classpath_data: Option<ClasspathNodeMetadata>,
}

/// Legacy discriminants for the V7 / V10 wire format.
pub(crate) const LEGACY_V7_KIND_MACRO: u8 = 0;
pub(crate) const LEGACY_V7_KIND_CLASSPATH: u8 = 1;
pub(crate) const LEGACY_V7_KIND_SYNTHETIC: u8 = 2;

/// Pre-U02 wire-format `NodeMetadataStore` payload.
///
/// Serializes / deserializes as `Vec<NodeMetadataEntryV10>` — the exact
/// shape master-tip wrote prior to U02. On V10 load, the
/// `upconvert_v10_to_v11` translator rebuilds a live `NodeMetadataStore`
/// by applying the variant mapping above to every entry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct NodeMetadataStoreV10 {
    pub(crate) entries: Vec<NodeMetadataEntryV10>,
}

/// Translate a pre-U02 [`NodeMetadataStoreV10`] into the live (V11)
/// `NodeMetadataStore` via the spec-mandated three-variant mapping.
///
/// Returns an error if any entry carries an unknown discriminant or a
/// missing required payload — the same strictness the V11 deserializer
/// applies for forward compatibility.
pub(crate) fn translate_metadata_store_v10_to_v11(
    v10: NodeMetadataStoreV10,
) -> Result<NodeMetadataStore, super::snapshot::PersistenceError> {
    use super::snapshot::PersistenceError;

    let mut store = NodeMetadataStore::new();
    for entry in v10.entries {
        let node_id = NodeId::new(entry.index, entry.generation);
        let stored = match entry.kind {
            LEGACY_V7_KIND_MACRO => {
                let payload = entry.macro_data.ok_or_else(|| {
                    PersistenceError::Serialization(format!(
                        "V10 metadata entry ({}, {}) declared `Macro` kind but carried \
                         no `macro_data` payload",
                        entry.index, entry.generation,
                    ))
                })?;
                StoredEntry {
                    typed: Some(TypedMetadata::Macro(payload)),
                    flags: NodeFlags::EMPTY,
                }
            }
            LEGACY_V7_KIND_CLASSPATH => {
                let payload = entry.classpath_data.ok_or_else(|| {
                    PersistenceError::Serialization(format!(
                        "V10 metadata entry ({}, {}) declared `Classpath` kind but \
                         carried no `classpath_data` payload",
                        entry.index, entry.generation,
                    ))
                })?;
                StoredEntry {
                    typed: Some(TypedMetadata::Classpath(payload)),
                    flags: NodeFlags::EMPTY,
                }
            }
            LEGACY_V7_KIND_SYNTHETIC => StoredEntry {
                typed: None,
                flags: NodeFlags::SYNTHETIC,
            },
            other => {
                return Err(PersistenceError::Serialization(format!(
                    "V10 metadata entry ({}, {}) carried unknown legacy kind \
                     discriminant {other}",
                    entry.index, entry.generation,
                )));
            }
        };
        store.insert_entry(node_id, stored);
    }
    Ok(store)
}

/// Reverse direction: build a V10 wire payload from a live
/// `NodeMetadataStore`. Used by the U03 V10-payload regression tests in
/// `snapshot.rs` to drive `write_framed_v10` with V10-shaped metadata.
///
/// Refuses to encode entries that carry both a typed payload **and** a
/// flag bit (U02 allows this co-occurrence; V10 does not), and refuses to
/// encode any flag bit other than `SYNTHETIC`.
pub(crate) fn translate_metadata_store_v11_to_v10(
    v11: &NodeMetadataStore,
) -> Result<NodeMetadataStoreV10, super::snapshot::PersistenceError> {
    use super::snapshot::PersistenceError;

    let mut entries = Vec::new();
    for ((index, generation), stored) in v11.iter_entries() {
        let (kind, macro_data, classpath_data) = match (&stored.typed, stored.flags) {
            (Some(TypedMetadata::Macro(m)), flags) if flags == NodeFlags::EMPTY => {
                (LEGACY_V7_KIND_MACRO, Some(m.clone()), None)
            }
            (Some(TypedMetadata::Classpath(c)), flags) if flags == NodeFlags::EMPTY => {
                (LEGACY_V7_KIND_CLASSPATH, None, Some(c.clone()))
            }
            (None, flags) if flags == NodeFlags::SYNTHETIC => {
                (LEGACY_V7_KIND_SYNTHETIC, None, None)
            }
            (typed, flags) => {
                return Err(PersistenceError::Serialization(format!(
                    "cannot encode V11 metadata entry ({index}, {generation}) into V10 \
                     wire format: typed={typed:?}, flags=0x{:02x} — V10 wire format only \
                     supports `Macro` / `Classpath` / `Synthetic` with no flag co-occurrence",
                    flags.bits(),
                )));
            }
        };
        entries.push(NodeMetadataEntryV10 {
            index,
            generation,
            kind,
            macro_data,
            classpath_data,
        });
    }
    Ok(NodeMetadataStoreV10 { entries })
}
