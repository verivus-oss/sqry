//! Test-only helpers for the unified graph.
//!
//! Implements DAG unit `U_WS1_12_PERSIST_RT` of the
//! `graph-fidelity-planner-correctness` plan (DESIGN §2.7 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`).
//!
//! The single public entry-point is [`canonical_arena`], a normaliser that
//! returns a [`CanonicalArena`] value uniquely determined by the *semantic*
//! content of a [`CodeGraph`] — independent of:
//!
//! * which `NodeId` slot any given node landed in (the load path may pack
//!   nodes into a different arena layout than the save path produced);
//! * which `StringId` slot the [`StringInterner`] assigned to any given
//!   interned string (the load path re-builds the interner table and may
//!   produce a different permutation, especially across V10/V11 upconverts);
//! * the order in which edges are stored.
//!
//! Two graphs that agree on every node's `(file, byte-span, kind, name)`,
//! every edge's `(canonical-source, canonical-target, kind discriminant,
//! canonical metadata)`, and on which `NodeFlags` bits each node carries
//! produce **identical** [`CanonicalArena`] values; conversely any semantic
//! difference shows up as inequality. This is the equivalence relation the
//! WS1 persistence round-trip test asserts: `save → load` must preserve it.
//!
//! # Gating
//!
//! The module is compiled under `cfg(any(test, feature = "test-support"))`
//! so it never leaks into release builds. The WS1 verification gate
//! exercises:
//!
//! ```text
//! cargo build --release -p sqry-cli
//! nm target/release/sqry | grep -c canonical_arena
//! # expected: 0
//! ```
//!
//! See module docs in `sqry-db/tests/property/persistence_roundtrip.rs` for
//! the full V11 round-trip property test that consumes this helper.
//!
//! # Algorithm
//!
//! 1. Build a `StringId → canonical-id` table by sorting the interner's
//!    occupied entries lexicographically and assigning dense indices.
//! 2. Build a `NodeId → CanonicalNodeId` table by sorting node entries on
//!    the *interner-permutation-invariant* key `(file_path, start_byte,
//!    end_byte, kind, canonical(name), canonical(qualified_name))` and
//!    assigning dense indices.
//! 3. Materialise a `BTreeMap<CanonicalNodeId, CanonicalNode>` whose values
//!    carry the same canonical key plus the node's flag bits (so changes
//!    to `address_taken` / `callsite_promiscuous` markers register).
//! 4. Materialise a `BTreeSet<CanonicalEdge>` whose key is `(canonical
//!    source, canonical target, kind discriminator string, canonical
//!    metadata)`. All `StringId`-bearing edge-metadata fields are remapped
//!    through the canonical string table; `Option<StringId>` becomes
//!    `Option<u32>`; raw primitives (booleans, integer enums) are
//!    serialised as their wire-stable representations.
//!
//! The output type is `Eq` and `Hash`. Two well-formed graphs that differ
//! only in arena slot ordering or interner permutation produce equal
//! values; any genuine data divergence (a dropped node, a re-pointed edge,
//! a flag change, a renamed symbol) breaks equality.

#![cfg(any(test, feature = "test-support"))]
// Variants of `CanonicalEdgeMetadata` mirror `EdgeKind` verbatim; the
// fields are documented by the parent enum in
// `sqry-core/src/graph/unified/edge/kind.rs`. Suppressing missing-docs
// here keeps the helper compact without losing referential clarity.
#![allow(missing_docs)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::kind::{EdgeKind, MqProtocol};
use crate::graph::unified::node::id::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::storage::metadata::NodeFlags;
use crate::graph::unified::string::id::StringId;

/// Compact, comparable form of an entire [`CodeGraph`] arena.
///
/// Equality of [`CanonicalArena`] is the test-equivalence relation defined
/// in the module docs: two graphs producing equal values are
/// indistinguishable to every query that does not inspect raw arena slot
/// indices or raw interner indices.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalArena {
    /// Number of distinct interned strings (after dedup). Pinned in the
    /// output so an interner that silently grew during save/load shows up
    /// as inequality rather than being masked by the remap.
    pub string_count: usize,
    /// Sorted list of every interned string. The position of a string in
    /// this vector is its canonical id.
    pub strings: Vec<String>,
    /// `NodeId.canonical_id() → CanonicalNode`. Keys are dense `[0,
    /// node_count)` after canonicalisation, so the map's `Ord` order is
    /// stable.
    pub nodes: BTreeMap<u32, CanonicalNode>,
    /// Every edge, in canonical form. `BTreeSet` so multi-edges (same
    /// source/target/kind) collapse — which is exactly what the
    /// `BidirectionalEdgeStore` does under the hood, so this is the
    /// correct identity for the round-trip property.
    pub edges: BTreeSet<CanonicalEdge>,
}

/// Canonical view of a single node.
///
/// Carries every interner-permutation-invariant piece of node identity that
/// the persistence layer must preserve, plus the `NodeFlags` markers (Phase
/// A C indirect-call metadata) so they round-trip correctly.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalNode {
    /// Canonical file path string (the POSIX-form path the
    /// `FileRegistry` knows the node by).
    pub file_path: String,
    /// Inclusive start byte offset.
    pub start_byte: u32,
    /// Exclusive end byte offset.
    pub end_byte: u32,
    /// Node kind discriminant (copy-typed enum, no interner remap needed).
    pub kind: NodeKind,
    /// Canonical name id (index into [`CanonicalArena::strings`]).
    pub name: u32,
    /// Canonical qualified-name id, when present.
    pub qualified_name: Option<u32>,
    /// `NodeFlags::SYNTHETIC` bit.
    pub synthetic: bool,
    /// `NodeFlags::ADDRESS_TAKEN` bit (Phase A C indirect-call marker).
    pub address_taken: bool,
    /// `NodeFlags::CALLSITE_PROMISCUOUS` bit (Phase A C indirect-call marker).
    pub callsite_promiscuous: bool,
}

/// Canonical view of a single edge.
///
/// `(canonical source, canonical target, edge-kind tag, canonical metadata)`
/// fully identifies an edge for round-trip purposes. The metadata is
/// flattened into a single `CanonicalEdgeMetadata` payload so the comparison
/// stays interner-independent.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalEdge {
    /// Canonical source node id.
    pub source: u32,
    /// Canonical target node id.
    pub target: u32,
    /// Edge-kind discriminator (one of the 38 variant tags). Stable across
    /// save/load by construction.
    pub kind: &'static str,
    /// Variant-specific payload, with every `StringId` remapped to a
    /// canonical id.
    pub metadata: CanonicalEdgeMetadata,
}

/// Variant-typed canonical edge metadata.
///
/// Every `StringId` is remapped to its canonical id; `Option<StringId>` to
/// `Option<u32>`. Copy-typed payloads (booleans, integer enums) are stored
/// directly. The enum mirrors `EdgeKind`'s 38 variants verbatim so a future
/// new variant produces a compile error here, forcing the helper to stay in
/// lockstep with the on-disk schema.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CanonicalEdgeMetadata {
    /// Variants with no payload — `Defines`, `Contains`, `References`,
    /// `Inherits`, `Implements`, `WebAssemblyCall`, plus the JVM
    /// classpath structural edges that all carry zero metadata.
    Empty,
    Calls {
        argument_count: u8,
        is_async: bool,
        resolved_via: u8,
    },
    Imports {
        alias: Option<u32>,
        is_wildcard: bool,
    },
    Exports {
        kind: u8,
        alias: Option<u32>,
    },
    TypeOf {
        context: Option<u8>,
        index: Option<u16>,
        name: Option<u32>,
    },
    LifetimeConstraint {
        constraint_kind: u8,
    },
    TraitMethodBinding {
        trait_name: u32,
        impl_type: u32,
        is_ambiguous: bool,
    },
    MacroExpansion {
        expansion_kind: u8,
        is_verified: bool,
    },
    FfiCall {
        convention: u8,
    },
    HttpRequest {
        method: u8,
        url: Option<u32>,
    },
    GrpcCall {
        service: u32,
        method: u32,
    },
    DbQuery {
        query_type: u8,
        table: Option<u32>,
    },
    TableRead {
        table_name: u32,
        schema: Option<u32>,
    },
    TableWrite {
        table_name: u32,
        schema: Option<u32>,
        operation: u8,
    },
    TriggeredBy {
        trigger_name: u32,
        schema: Option<u32>,
    },
    MessageQueue {
        protocol_tag: u8,
        protocol_other: Option<u32>,
        topic: Option<u32>,
    },
    WebSocket {
        event: Option<u32>,
    },
    GraphQLOperation {
        operation: u32,
    },
    ProcessExec {
        command: u32,
    },
    FileIpc {
        path_pattern: Option<u32>,
    },
    ProtocolCall {
        protocol: u32,
        metadata: Option<u32>,
    },
}

/// Builds a [`CanonicalArena`] from a live [`CodeGraph`].
///
/// See the module docs for the equivalence relation this normalises against.
///
/// # Determinism
///
/// Given any two `CodeGraph` values `g1` and `g2` that differ only in arena
/// slot ordering, in interner permutation, or in edge insertion order,
/// `canonical_arena(&g1) == canonical_arena(&g2)`. Any semantic difference
/// — a missing node, a re-pointed edge, a flipped flag bit, a different
/// span — breaks equality.
///
/// # Panics
///
/// Panics if a node references a `StringId` that does not resolve in the
/// interner. Such graphs are not constructible by the public APIs; this is
/// a programmer-error path for the test surface.
#[must_use]
pub fn canonical_arena(graph: &CodeGraph) -> CanonicalArena {
    let snapshot = graph.snapshot();

    // -----------------------------------------------------------------
    // Step 1 — canonical string table.
    //
    // Sort the (StringId, &Arc<str>) pairs the interner exposes by the
    // string value, then assign dense canonical ids. We carry both the
    // raw StringId (so we can build the remap) and the owned String (so
    // the output is self-contained).
    // -----------------------------------------------------------------
    let interner = snapshot.strings();
    let mut interned: Vec<(StringId, String)> = interner
        .iter()
        .map(|(id, s)| (id, s.as_ref().to_owned()))
        .collect();
    interned.sort_by(|a, b| a.1.cmp(&b.1));

    let mut string_remap: HashMap<StringId, u32> = HashMap::with_capacity(interned.len());
    let mut strings: Vec<String> = Vec::with_capacity(interned.len());
    for (idx, (id, s)) in interned.into_iter().enumerate() {
        let canonical = u32::try_from(idx).expect("canonical string index fits in u32");
        string_remap.insert(id, canonical);
        strings.push(s);
    }

    let remap_required = |id: StringId| -> u32 {
        *string_remap
            .get(&id)
            .unwrap_or_else(|| panic!("StringId {id:?} does not resolve in the interner"))
    };
    let remap_optional = |id: Option<StringId>| -> Option<u32> {
        id.map(|inner| {
            *string_remap
                .get(&inner)
                .unwrap_or_else(|| panic!("Optional StringId {inner:?} does not resolve"))
        })
    };

    // -----------------------------------------------------------------
    // Step 2 — canonical node table.
    //
    // Collect (NodeId, CanonicalNode) pairs, then sort by the canonical
    // key so we can hand out dense canonical ids. Sorting is on the full
    // `CanonicalNode` value because every component of the key is part
    // of the equivalence relation; ties on (file, span, kind, name)
    // would represent two genuinely identical nodes — which is rare in
    // practice but still well-defined.
    // -----------------------------------------------------------------
    let files = snapshot.files();
    let metadata = snapshot.macro_metadata();
    let mut node_pairs: Vec<(NodeId, CanonicalNode)> = Vec::new();
    for (node_id, entry) in snapshot.iter_nodes() {
        let file_path = files
            .iter()
            .find_map(|(fid, path)| {
                (fid == entry.file).then(|| path.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        let flags = metadata.get_flags(node_id);
        node_pairs.push((
            node_id,
            CanonicalNode {
                file_path,
                start_byte: entry.start_byte,
                end_byte: entry.end_byte,
                kind: entry.kind,
                name: remap_required(entry.name),
                qualified_name: remap_optional(entry.qualified_name),
                synthetic: flags.contains(NodeFlags::SYNTHETIC),
                address_taken: flags.contains(NodeFlags::ADDRESS_TAKEN),
                callsite_promiscuous: flags.contains(NodeFlags::CALLSITE_PROMISCUOUS),
            },
        ));
    }
    node_pairs.sort_by(|a, b| a.1.cmp(&b.1));

    let mut node_remap: HashMap<NodeId, u32> = HashMap::with_capacity(node_pairs.len());
    let mut nodes: BTreeMap<u32, CanonicalNode> = BTreeMap::new();
    for (canonical_idx, (node_id, canonical)) in node_pairs.into_iter().enumerate() {
        let canonical_id = u32::try_from(canonical_idx).expect("canonical node index fits in u32");
        node_remap.insert(node_id, canonical_id);
        nodes.insert(canonical_id, canonical);
    }

    // -----------------------------------------------------------------
    // Step 3 — canonical edges.
    //
    // BTreeSet so duplicate `(source, target, kind, metadata)` tuples
    // collapse to one entry. That mirrors the deduplication
    // `BidirectionalEdgeStore` performs, so the property test is
    // measuring the right invariant. Edges to/from nodes outside the
    // remap (which can only happen if the snapshot is malformed) are
    // dropped — the well-formed-graph generator never produces them.
    // -----------------------------------------------------------------
    let mut edges: BTreeSet<CanonicalEdge> = BTreeSet::new();
    for (src, tgt, kind) in snapshot.iter_edges() {
        let (Some(&source), Some(&target)) = (node_remap.get(&src), node_remap.get(&tgt)) else {
            continue;
        };
        let (tag, metadata) = canonicalise_edge_kind(&kind, &remap_required, &remap_optional);
        edges.insert(CanonicalEdge {
            source,
            target,
            kind: tag,
            metadata,
        });
    }

    CanonicalArena {
        string_count: strings.len(),
        strings,
        nodes,
        edges,
    }
}

/// Splits an `EdgeKind` into its variant tag plus a fully remapped
/// `CanonicalEdgeMetadata`.
///
/// Centralises the per-variant translation so a future enum addition
/// produces a single compile error here, rather than silently dropping the
/// new variant from the canonical form.
fn canonicalise_edge_kind(
    kind: &EdgeKind,
    req: &impl Fn(StringId) -> u32,
    opt: &impl Fn(Option<StringId>) -> Option<u32>,
) -> (&'static str, CanonicalEdgeMetadata) {
    match kind {
        EdgeKind::Defines => ("Defines", CanonicalEdgeMetadata::Empty),
        EdgeKind::Contains => ("Contains", CanonicalEdgeMetadata::Empty),
        EdgeKind::Calls {
            argument_count,
            is_async,
            resolved_via,
        } => (
            "Calls",
            CanonicalEdgeMetadata::Calls {
                argument_count: *argument_count,
                is_async: *is_async,
                resolved_via: *resolved_via as u8,
            },
        ),
        EdgeKind::References => ("References", CanonicalEdgeMetadata::Empty),
        EdgeKind::Imports { alias, is_wildcard } => (
            "Imports",
            CanonicalEdgeMetadata::Imports {
                alias: opt(*alias),
                is_wildcard: *is_wildcard,
            },
        ),
        EdgeKind::Exports { kind, alias } => (
            "Exports",
            CanonicalEdgeMetadata::Exports {
                kind: *kind as u8,
                alias: opt(*alias),
            },
        ),
        EdgeKind::TypeOf {
            context,
            index,
            name,
        } => (
            "TypeOf",
            CanonicalEdgeMetadata::TypeOf {
                context: context.map(|c| c as u8),
                index: *index,
                name: opt(*name),
            },
        ),
        EdgeKind::Inherits => ("Inherits", CanonicalEdgeMetadata::Empty),
        EdgeKind::Implements => ("Implements", CanonicalEdgeMetadata::Empty),
        EdgeKind::LifetimeConstraint { constraint_kind } => (
            "LifetimeConstraint",
            CanonicalEdgeMetadata::LifetimeConstraint {
                constraint_kind: *constraint_kind as u8,
            },
        ),
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => (
            "TraitMethodBinding",
            CanonicalEdgeMetadata::TraitMethodBinding {
                trait_name: req(*trait_name),
                impl_type: req(*impl_type),
                is_ambiguous: *is_ambiguous,
            },
        ),
        EdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        } => (
            "MacroExpansion",
            CanonicalEdgeMetadata::MacroExpansion {
                expansion_kind: *expansion_kind as u8,
                is_verified: *is_verified,
            },
        ),
        EdgeKind::FfiCall { convention } => (
            "FfiCall",
            CanonicalEdgeMetadata::FfiCall {
                convention: *convention as u8,
            },
        ),
        EdgeKind::HttpRequest { method, url } => (
            "HttpRequest",
            CanonicalEdgeMetadata::HttpRequest {
                method: *method as u8,
                url: opt(*url),
            },
        ),
        EdgeKind::GrpcCall { service, method } => (
            "GrpcCall",
            CanonicalEdgeMetadata::GrpcCall {
                service: req(*service),
                method: req(*method),
            },
        ),
        EdgeKind::WebAssemblyCall => ("WebAssemblyCall", CanonicalEdgeMetadata::Empty),
        EdgeKind::DbQuery { query_type, table } => (
            "DbQuery",
            CanonicalEdgeMetadata::DbQuery {
                query_type: *query_type as u8,
                table: opt(*table),
            },
        ),
        EdgeKind::TableRead { table_name, schema } => (
            "TableRead",
            CanonicalEdgeMetadata::TableRead {
                table_name: req(*table_name),
                schema: opt(*schema),
            },
        ),
        EdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        } => (
            "TableWrite",
            CanonicalEdgeMetadata::TableWrite {
                table_name: req(*table_name),
                schema: opt(*schema),
                operation: *operation as u8,
            },
        ),
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => (
            "TriggeredBy",
            CanonicalEdgeMetadata::TriggeredBy {
                trigger_name: req(*trigger_name),
                schema: opt(*schema),
            },
        ),
        EdgeKind::MessageQueue { protocol, topic } => {
            let (protocol_tag, protocol_other) = match protocol {
                MqProtocol::Kafka => (0u8, None),
                MqProtocol::Sqs => (1u8, None),
                MqProtocol::RabbitMq => (2u8, None),
                MqProtocol::Nats => (3u8, None),
                MqProtocol::Redis => (4u8, None),
                MqProtocol::Other(id) => (5u8, Some(req(*id))),
            };
            (
                "MessageQueue",
                CanonicalEdgeMetadata::MessageQueue {
                    protocol_tag,
                    protocol_other,
                    topic: opt(*topic),
                },
            )
        }
        EdgeKind::WebSocket { event } => (
            "WebSocket",
            CanonicalEdgeMetadata::WebSocket { event: opt(*event) },
        ),
        EdgeKind::GraphQLOperation { operation } => (
            "GraphQLOperation",
            CanonicalEdgeMetadata::GraphQLOperation {
                operation: req(*operation),
            },
        ),
        EdgeKind::ProcessExec { command } => (
            "ProcessExec",
            CanonicalEdgeMetadata::ProcessExec {
                command: req(*command),
            },
        ),
        EdgeKind::FileIpc { path_pattern } => (
            "FileIpc",
            CanonicalEdgeMetadata::FileIpc {
                path_pattern: opt(*path_pattern),
            },
        ),
        EdgeKind::ProtocolCall { protocol, metadata } => (
            "ProtocolCall",
            CanonicalEdgeMetadata::ProtocolCall {
                protocol: req(*protocol),
                metadata: opt(*metadata),
            },
        ),
        EdgeKind::GenericBound => ("GenericBound", CanonicalEdgeMetadata::Empty),
        EdgeKind::AnnotatedWith => ("AnnotatedWith", CanonicalEdgeMetadata::Empty),
        EdgeKind::AnnotationParam => ("AnnotationParam", CanonicalEdgeMetadata::Empty),
        EdgeKind::LambdaCaptures => ("LambdaCaptures", CanonicalEdgeMetadata::Empty),
        EdgeKind::ModuleExports => ("ModuleExports", CanonicalEdgeMetadata::Empty),
        EdgeKind::ModuleRequires => ("ModuleRequires", CanonicalEdgeMetadata::Empty),
        EdgeKind::ModuleOpens => ("ModuleOpens", CanonicalEdgeMetadata::Empty),
        EdgeKind::ModuleProvides => ("ModuleProvides", CanonicalEdgeMetadata::Empty),
        EdgeKind::TypeArgument => ("TypeArgument", CanonicalEdgeMetadata::Empty),
        EdgeKind::ExtensionReceiver => ("ExtensionReceiver", CanonicalEdgeMetadata::Empty),
        EdgeKind::CompanionOf => ("CompanionOf", CanonicalEdgeMetadata::Empty),
        EdgeKind::SealedPermit => ("SealedPermit", CanonicalEdgeMetadata::Empty),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::graph::Language;
    use crate::graph::unified::edge::kind::{EdgeKind, HttpMethod};
    use crate::graph::unified::node::kind::NodeKind;
    use crate::graph::unified::storage::arena::NodeEntry;

    /// Smoke test: an empty graph normalises to an empty `CanonicalArena`.
    #[test]
    fn empty_graph_normalises_to_empty() {
        let graph = CodeGraph::new();
        let canon = canonical_arena(&graph);
        assert!(canon.strings.is_empty());
        assert!(canon.nodes.is_empty());
        assert!(canon.edges.is_empty());
        assert_eq!(canon.string_count, 0);
    }

    /// Builds a small graph with two nodes + one Calls edge + one
    /// HttpRequest edge (interner-carrying metadata). The canonical form
    /// must be stable across snapshot clones.
    #[test]
    fn canonical_form_is_stable_on_self() {
        let graph = build_demo_graph();
        let a = canonical_arena(&graph);
        let b = canonical_arena(&graph);
        assert_eq!(a, b);
        assert_eq!(a.nodes.len(), 2);
        assert_eq!(a.edges.len(), 2);
    }

    /// Save → load round-trip with the V11 persistence path produces an
    /// equal canonical arena. This is the regression-shape test that the
    /// proptest in `sqry-db/tests/property/persistence_roundtrip.rs`
    /// generalises.
    #[test]
    fn save_load_roundtrip_yields_equal_canonical_arena() {
        let graph = build_demo_graph();
        let canon_before = canonical_arena(&graph);

        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("snapshot.sqry");
        crate::graph::unified::persistence::save_to_path(&graph, &path).expect("save");
        let reloaded =
            crate::graph::unified::persistence::load_from_path(&path, None).expect("load");

        let canon_after = canonical_arena(&reloaded);
        assert_eq!(canon_before, canon_after);
    }

    /// Negative test: mutating a node's flag bit changes the canonical
    /// arena. Confirms the helper is not vacuously equal.
    #[test]
    fn flag_change_breaks_equality() {
        let mut graph = CodeGraph::new();
        let file = graph
            .files_mut()
            .register_with_language(&PathBuf::from("demo.rs"), Some(Language::Rust))
            .expect("register file");
        let name = graph.strings_mut().intern("foo").expect("intern");
        let entry = NodeEntry::new(NodeKind::Function, name, file)
            .with_qualified_name(name)
            .with_byte_range(0, 16);
        let nid = graph.nodes_mut().alloc(entry).expect("alloc");
        graph
            .indices_mut()
            .add(nid, NodeKind::Function, name, Some(name), file);
        let before = canonical_arena(&graph);
        graph.macro_metadata_mut().mark_address_taken(nid);
        let after = canonical_arena(&graph);
        assert_ne!(before, after);
    }

    fn build_demo_graph() -> Arc<CodeGraph> {
        let mut graph = CodeGraph::new();
        let file = graph
            .files_mut()
            .register_with_language(&PathBuf::from("demo.rs"), Some(Language::Rust))
            .expect("register file");
        let n_main = graph.strings_mut().intern("main").expect("intern main");
        let n_target = graph.strings_mut().intern("target").expect("intern target");
        let url = graph
            .strings_mut()
            .intern("https://example/x")
            .expect("intern url");
        let main_entry = NodeEntry::new(NodeKind::Function, n_main, file)
            .with_qualified_name(n_main)
            .with_byte_range(0, 16);
        let tgt_entry = NodeEntry::new(NodeKind::Function, n_target, file)
            .with_qualified_name(n_target)
            .with_byte_range(32, 48);
        let main_id = graph.nodes_mut().alloc(main_entry).expect("alloc main");
        let tgt_id = graph.nodes_mut().alloc(tgt_entry).expect("alloc target");
        graph
            .indices_mut()
            .add(main_id, NodeKind::Function, n_main, Some(n_main), file);
        graph
            .indices_mut()
            .add(tgt_id, NodeKind::Function, n_target, Some(n_target), file);
        graph.edges().add_edge(
            main_id,
            tgt_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: crate::graph::unified::edge::kind::ResolvedVia::Direct,
            },
            file,
        );
        graph.edges().add_edge(
            main_id,
            tgt_id,
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: Some(url),
            },
            file,
        );
        Arc::new(graph)
    }
}
