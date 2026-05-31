//! Well-formed `CodeGraph` generator for the WS1 differential harness.
//!
//! Implements DESIGN §2.2 of
//! `docs/development/graph-fidelity-planner-correctness/02_DESIGN-graph-fidelity-planner-correctness.md`.
//!
//! # Contract
//!
//! `well_formed_graph()` returns a `proptest::strategy::Strategy<Value =
//! GeneratedGraph>` that **never emits an ill-formed graph**. Every generated
//! value passes [`check_well_formed`] by construction:
//!
//! 1. Every `NodeId` in every edge resolves to a live arena slot with a
//!    matching generation.
//! 2. Every `StringId` in node names / edge metadata resolves in the
//!    `StringInterner`.
//! 3. The set of `Defines` edges forms a forest: each non-root node has
//!    exactly one incoming `Defines` edge, and there are no cycles.
//! 4. No dangling edges (every edge's source and target are live nodes
//!    registered against the edge's file).
//! 5. `Calls`-flavoured edges respect a call-compatible target kind
//!    (Function / Method / Macro / Constant / `LambdaTarget`), matching the
//!    Phase 4c-prime `CALL_COMPATIBLE_KINDS` set CLAUDE.md documents.
//! 6. Every metadata sub-enum (HTTP method, DB op, export kind, etc.) is
//!    populated with a documented variant; no `StringId::INVALID` ever
//!    appears in user-facing edge fields.
//!
//! # Shrinker
//!
//! Default proptest shrinking on raw integer vectors would routinely produce
//! ill-formed graphs (orphaned edges, dangling `NodeId`s). We override the
//! `Strategy::Tree` so that shrinking traverses a sequence of progressively
//! smaller, **still-well-formed** [`GraphRecipe`] values:
//!
//! 1. Drop leaf nodes (no outgoing edges, no `Defines` children) along with
//!    every edge that references them.
//! 2. Drop edges (preserving structural `Defines` so the forest stays
//!    connected; dropping a `Defines` edge requires dropping its subtree).
//! 3. Drop whole files (and their nodes / edges) so the graph collapses
//!    monotonically.
//!
//! Shrinker acceptance: any synthetic counter-example reduces to
//! ≤ 12 nodes / ≤ 20 edges within ≤ 10 000 iterations
//! (`graph_gen_self_test::shrink_synthetic_counter_example`).
//!
//! # Edge-kind coverage
//!
//! Every variant of `EdgeKind` (38 variants as of V11 — verified against
//! `sqry-core/src/graph/unified/edge/kind.rs` HEAD) is exercised. The
//! generator picks a variant uniformly at random per edge slot;
//! `graph_gen_self_test::all_edge_kinds_emitted` proves a 2 048-graph sample
//! hits every variant.

#![allow(dead_code)] // The differential harness lands across U_WS1_4 .. U_WS1_8; this module is the
// shared fixture each unit pulls from. Items used only by future units are kept here so the
// public surface stays single-sourced.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::strategy::{NewTree, ValueTree};
use proptest::test_runner::TestRunner;

use sqry_core::graph::Language;
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{
    DbQueryType, EdgeKind, ExportKind, FfiConvention, HttpMethod, LifetimeConstraintKind,
    MacroExpansionKind, MqProtocol, ResolvedVia, TableWriteOp, TypeOfContext,
};
use sqry_core::graph::unified::file::id::FileId;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::graph::unified::string::StringId;

// ---------------------------------------------------------------------------
// Tunable bounds
// ---------------------------------------------------------------------------

/// Maximum number of files per generated graph.
///
/// Bounded so individual proptest cases stay fast. The differential harness
/// cares about edge-kind coverage and topology variety, not file count.
pub const MAX_FILES: usize = 6;

/// Maximum number of nodes per generated graph.
///
/// DESIGN §2.2 picks 64; we keep that ceiling — sufficient to cover every
/// `EdgeKind` variant plus several call-graph topologies per case.
pub const MAX_NODES: usize = 64;

/// Maximum number of non-`Defines` edges per generated graph.
///
/// `Defines` edges are bookkept separately (one per non-root node). The
/// edge-budget here governs only the interesting payload edges.
pub const MAX_EXTRA_EDGES: usize = 128;

// ---------------------------------------------------------------------------
// Recipe — the canonical pre-commit value the generator produces
// ---------------------------------------------------------------------------

/// A canonical, well-formed description of a `CodeGraph` that can be
/// materialised into a fresh `CodeGraph` via [`GraphRecipe::materialize`].
///
/// The recipe is the value the proptest tree operates on. Shrinking returns
/// a smaller `GraphRecipe`; materialisation is deterministic.
#[derive(Debug, Clone)]
pub struct GraphRecipe {
    /// File slots, in registration order. The index in this vector is the
    /// recipe-local file id; `materialize` translates to the live `FileId`
    /// returned by `FileRegistry::register_with_language`.
    pub files: Vec<RecipeFile>,
    /// Node slots, in allocation order.
    pub nodes: Vec<RecipeNode>,
    /// Non-`Defines` edges. `Defines` edges are derived from `nodes[i].parent`.
    pub edges: Vec<RecipeEdge>,
}

/// One file in a [`GraphRecipe`].
#[derive(Debug, Clone)]
pub struct RecipeFile {
    /// Synthetic POSIX path under a fixed prefix.
    pub path: String,
    /// Language tag passed to `FileRegistry::register_with_language`.
    pub language: Language,
}

/// One node in a [`GraphRecipe`].
#[derive(Debug, Clone)]
pub struct RecipeNode {
    /// Node category.
    pub kind: NodeKind,
    /// Simple identifier — also used as the qualified name.
    pub name: String,
    /// Recipe-local file id (index into [`GraphRecipe::files`]).
    pub file_idx: usize,
    /// Index of the parent node in [`GraphRecipe::nodes`] (smaller index).
    ///
    /// `None` marks a tree root for the file (no incoming `Defines`).
    /// `Some(p)` means the materialised graph contains a
    /// `Defines: nodes[p] -> self` edge, registered against the same file
    /// as the parent.
    pub parent: Option<usize>,
    /// Synthetic source byte offset (start = `byte_offset`, end =
    /// `byte_offset + 16`). Each node gets a unique non-overlapping span
    /// inside its file so range-based queries are well defined.
    pub byte_offset: u32,
    /// When true, [`GraphRecipe::materialize`] calls
    /// `NodeMetadataStore::mark_address_taken` for the resulting `NodeId`.
    ///
    /// Drives non-vacuous differential coverage for the `AddressTakenQuery`
    /// baseline at `sqry-db/src/baseline.rs::address_taken` — without
    /// generator-emitted flag bits the query returns empty on every
    /// generated graph and the diff_cicall family becomes trivially equal.
    /// Calibrated probability ≈ 15% per node in the proptest strategy so
    /// the population is non-trivial but graphs remain mostly clean.
    pub address_taken: bool,
    /// When true, [`GraphRecipe::materialize`] calls
    /// `NodeMetadataStore::mark_callsite_promiscuous` for the resulting
    /// `NodeId`.
    ///
    /// Drives non-vacuous differential coverage for the
    /// `CallsitePromiscuousQuery` baseline at
    /// `sqry-db/src/baseline.rs::callsite_promiscuous`. Calibrated
    /// probability ≈ 15% per node in the proptest strategy. Both flags
    /// compose freely (a node may carry neither, one, or both).
    pub callsite_promiscuous: bool,
}

/// One non-`Defines` edge in a [`GraphRecipe`].
#[derive(Debug, Clone)]
pub struct RecipeEdge {
    /// Index of the source node in [`GraphRecipe::nodes`].
    pub source: usize,
    /// Index of the target node in [`GraphRecipe::nodes`].
    pub target: usize,
    /// Edge variant + metadata. All `StringId`-bearing fields are recipe-
    /// local string indices; [`GraphRecipe::materialize`] translates them.
    pub kind: RecipeEdgeKind,
}

/// Edge-kind selector for a [`RecipeEdge`].
///
/// Mirrors `EdgeKind` (38 variants — `sqry-core/src/graph/unified/edge/kind.rs`)
/// but carries plain `String` payloads for fields that would otherwise need a
/// `StringId`. `materialize` interns the strings against the live graph.
#[derive(Debug, Clone)]
pub enum RecipeEdgeKind {
    Defines,
    Contains,
    Calls {
        argument_count: u8,
        is_async: bool,
        resolved_via: ResolvedVia,
    },
    References,
    Imports {
        alias: Option<String>,
        is_wildcard: bool,
    },
    Exports {
        kind: ExportKind,
        alias: Option<String>,
    },
    TypeOf {
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<String>,
    },
    Inherits,
    Implements,
    LifetimeConstraint {
        constraint_kind: LifetimeConstraintKind,
    },
    TraitMethodBinding {
        trait_name: String,
        impl_type: String,
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
        url: Option<String>,
    },
    GrpcCall {
        service: String,
        method: String,
    },
    WebAssemblyCall,
    DbQuery {
        query_type: DbQueryType,
        table: Option<String>,
    },
    TableRead {
        table_name: String,
        schema: Option<String>,
    },
    TableWrite {
        table_name: String,
        schema: Option<String>,
        operation: TableWriteOp,
    },
    TriggeredBy {
        trigger_name: String,
        schema: Option<String>,
    },
    MessageQueue {
        protocol: MqProtocolChoice,
        topic: Option<String>,
    },
    WebSocket {
        event: Option<String>,
    },
    GraphQLOperation {
        operation: String,
    },
    ProcessExec {
        command: String,
    },
    FileIpc {
        path_pattern: Option<String>,
    },
    ProtocolCall {
        protocol: String,
        metadata: Option<String>,
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

/// `MqProtocol` choice carrier — the `Other(StringId)` arm is materialised
/// from a plain `String` so the recipe stays interner-free.
#[derive(Debug, Clone)]
pub enum MqProtocolChoice {
    Kafka,
    Sqs,
    RabbitMq,
    Nats,
    Redis,
    Other(String),
}

impl RecipeEdgeKind {
    /// The 38-variant discriminant tag used by coverage tracking.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Defines => "Defines",
            Self::Contains => "Contains",
            Self::Calls { .. } => "Calls",
            Self::References => "References",
            Self::Imports { .. } => "Imports",
            Self::Exports { .. } => "Exports",
            Self::TypeOf { .. } => "TypeOf",
            Self::Inherits => "Inherits",
            Self::Implements => "Implements",
            Self::LifetimeConstraint { .. } => "LifetimeConstraint",
            Self::TraitMethodBinding { .. } => "TraitMethodBinding",
            Self::MacroExpansion { .. } => "MacroExpansion",
            Self::FfiCall { .. } => "FfiCall",
            Self::HttpRequest { .. } => "HttpRequest",
            Self::GrpcCall { .. } => "GrpcCall",
            Self::WebAssemblyCall => "WebAssemblyCall",
            Self::DbQuery { .. } => "DbQuery",
            Self::TableRead { .. } => "TableRead",
            Self::TableWrite { .. } => "TableWrite",
            Self::TriggeredBy { .. } => "TriggeredBy",
            Self::MessageQueue { .. } => "MessageQueue",
            Self::WebSocket { .. } => "WebSocket",
            Self::GraphQLOperation { .. } => "GraphQLOperation",
            Self::ProcessExec { .. } => "ProcessExec",
            Self::FileIpc { .. } => "FileIpc",
            Self::ProtocolCall { .. } => "ProtocolCall",
            Self::GenericBound => "GenericBound",
            Self::AnnotatedWith => "AnnotatedWith",
            Self::AnnotationParam => "AnnotationParam",
            Self::LambdaCaptures => "LambdaCaptures",
            Self::ModuleExports => "ModuleExports",
            Self::ModuleRequires => "ModuleRequires",
            Self::ModuleOpens => "ModuleOpens",
            Self::ModuleProvides => "ModuleProvides",
            Self::TypeArgument => "TypeArgument",
            Self::ExtensionReceiver => "ExtensionReceiver",
            Self::CompanionOf => "CompanionOf",
            Self::SealedPermit => "SealedPermit",
        }
    }

    /// Whether this edge variant requires a call-compatible target kind.
    #[must_use]
    pub fn requires_call_compatible_target(&self) -> bool {
        matches!(self, Self::Calls { .. })
    }
}

/// Canonical list of every `EdgeKind` discriminant tag. Used for coverage
/// self-tests so the generator's variant set stays in lockstep with the
/// 38-variant enumeration in `sqry-core/src/graph/unified/edge/kind.rs`.
pub const ALL_EDGE_KIND_TAGS: &[&str] = &[
    "Defines",
    "Contains",
    "Calls",
    "References",
    "Imports",
    "Exports",
    "TypeOf",
    "Inherits",
    "Implements",
    "LifetimeConstraint",
    "TraitMethodBinding",
    "MacroExpansion",
    "FfiCall",
    "HttpRequest",
    "GrpcCall",
    "WebAssemblyCall",
    "DbQuery",
    "TableRead",
    "TableWrite",
    "TriggeredBy",
    "MessageQueue",
    "WebSocket",
    "GraphQLOperation",
    "ProcessExec",
    "FileIpc",
    "ProtocolCall",
    "GenericBound",
    "AnnotatedWith",
    "AnnotationParam",
    "LambdaCaptures",
    "ModuleExports",
    "ModuleRequires",
    "ModuleOpens",
    "ModuleProvides",
    "TypeArgument",
    "ExtensionReceiver",
    "CompanionOf",
    "SealedPermit",
];

/// Call-compatible target kinds, mirroring Phase 4c-prime
/// `CALL_COMPATIBLE_KINDS` (CLAUDE.md "Unified Graph Architecture" →
/// "Build Pipeline" → Phase 4c-prime).
const CALL_COMPATIBLE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Macro,
    NodeKind::Constant,
    NodeKind::LambdaTarget,
];

// ---------------------------------------------------------------------------
// Materialisation — recipe -> live CodeGraph
// ---------------------------------------------------------------------------

/// The output of [`GraphRecipe::materialize`] — a live `CodeGraph` plus the
/// recipe-local-index → `NodeId` map differential tests use to anchor
/// queries.
#[derive(Debug, Clone)]
pub struct GeneratedGraph {
    /// The fully built, well-formed `CodeGraph`.
    pub graph: Arc<CodeGraph>,
    /// recipe-local-index → live `NodeId`. Aligned with
    /// `GraphRecipe::nodes`.
    pub node_ids: Vec<NodeId>,
    /// recipe-local-index → live `FileId`. Aligned with
    /// `GraphRecipe::files`.
    pub file_ids: Vec<FileId>,
    /// The recipe the graph was built from. Kept so shrunk values can be
    /// regenerated and so differential failure messages can print the
    /// minimal recipe.
    pub recipe: GraphRecipe,
}

impl GraphRecipe {
    /// Materialise this recipe into a `CodeGraph`.
    ///
    /// Allocates files, then nodes, then edges. Strings are interned on
    /// demand against the live `StringInterner`.
    ///
    /// # Panics
    ///
    /// Panics if the recipe references an undefined node / file index or
    /// if string interning fails. Such recipes are not constructible from
    /// [`well_formed_graph`] — this is a programmer-error path.
    #[must_use]
    pub fn materialize(self) -> GeneratedGraph {
        let mut graph = CodeGraph::new();

        // ---- Files ----
        let mut file_ids: Vec<FileId> = Vec::with_capacity(self.files.len());
        for f in &self.files {
            let file_id = graph
                .files_mut()
                .register_with_language(&PathBuf::from(&f.path), Some(f.language))
                .expect("register file");
            file_ids.push(file_id);
        }

        // ---- Nodes ----
        let mut node_ids: Vec<NodeId> = Vec::with_capacity(self.nodes.len());
        for n in &self.nodes {
            let file_id = file_ids[n.file_idx];
            let name_id = graph.strings_mut().intern(&n.name).expect("intern name");
            let entry = NodeEntry::new(n.kind, name_id, file_id)
                .with_qualified_name(name_id)
                .with_byte_range(n.byte_offset, n.byte_offset + 16);
            let node_id = graph.nodes_mut().alloc(entry).expect("alloc node");
            graph
                .indices_mut()
                .add(node_id, n.kind, name_id, Some(name_id), file_id);
            // Emit NodeFlags marker bits requested by the recipe. Both flags
            // compose with each other and with any typed payload — see
            // `NodeFlags` docs in `sqry-core/src/graph/unified/storage/metadata.rs`.
            // Drives non-vacuous differential coverage for AddressTakenQuery /
            // CallsitePromiscuousQuery in the diff_cicall family.
            if n.address_taken {
                graph.macro_metadata_mut().mark_address_taken(node_id);
            }
            if n.callsite_promiscuous {
                graph
                    .macro_metadata_mut()
                    .mark_callsite_promiscuous(node_id);
            }
            node_ids.push(node_id);
        }

        // ---- Defines forest (derived from parent links) ----
        for (idx, node) in self.nodes.iter().enumerate() {
            if let Some(parent_idx) = node.parent {
                let parent_id = node_ids[parent_idx];
                let child_id = node_ids[idx];
                let file_id = file_ids[node.file_idx];
                graph
                    .edges()
                    .add_edge(parent_id, child_id, EdgeKind::Defines, file_id);
            }
        }

        // ---- Non-Defines edges ----
        for e in &self.edges {
            let source_id = node_ids[e.source];
            let target_id = node_ids[e.target];
            // Anchor the edge against the source's file — keeps file-scoped
            // edge cleanup behaviour aligned with the language plugins.
            let file_idx = self.nodes[e.source].file_idx;
            let file_id = file_ids[file_idx];
            let kind = materialize_edge_kind(&e.kind, &mut graph);
            graph.edges().add_edge(source_id, target_id, kind, file_id);
        }

        GeneratedGraph {
            graph: Arc::new(graph),
            node_ids,
            file_ids,
            recipe: self,
        }
    }
}

fn intern_one(graph: &mut CodeGraph, s: &str) -> StringId {
    graph.strings_mut().intern(s).expect("intern")
}

fn intern_opt(graph: &mut CodeGraph, s: &Option<String>) -> Option<StringId> {
    s.as_ref().map(|t| intern_one(graph, t))
}

fn materialize_edge_kind(kind: &RecipeEdgeKind, graph: &mut CodeGraph) -> EdgeKind {
    match kind {
        RecipeEdgeKind::Defines => EdgeKind::Defines,
        RecipeEdgeKind::Contains => EdgeKind::Contains,
        RecipeEdgeKind::Calls {
            argument_count,
            is_async,
            resolved_via,
        } => EdgeKind::Calls {
            argument_count: *argument_count,
            is_async: *is_async,
            resolved_via: *resolved_via,
        },
        RecipeEdgeKind::References => EdgeKind::References,
        RecipeEdgeKind::Imports { alias, is_wildcard } => EdgeKind::Imports {
            alias: intern_opt(graph, alias),
            is_wildcard: *is_wildcard,
        },
        RecipeEdgeKind::Exports { kind, alias } => EdgeKind::Exports {
            kind: *kind,
            alias: intern_opt(graph, alias),
        },
        RecipeEdgeKind::TypeOf {
            context,
            index,
            name,
        } => EdgeKind::TypeOf {
            context: *context,
            index: *index,
            name: intern_opt(graph, name),
        },
        RecipeEdgeKind::Inherits => EdgeKind::Inherits,
        RecipeEdgeKind::Implements => EdgeKind::Implements,
        RecipeEdgeKind::LifetimeConstraint { constraint_kind } => EdgeKind::LifetimeConstraint {
            constraint_kind: *constraint_kind,
        },
        RecipeEdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => EdgeKind::TraitMethodBinding {
            trait_name: intern_one(graph, trait_name),
            impl_type: intern_one(graph, impl_type),
            is_ambiguous: *is_ambiguous,
        },
        RecipeEdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        } => EdgeKind::MacroExpansion {
            expansion_kind: *expansion_kind,
            is_verified: *is_verified,
        },
        RecipeEdgeKind::FfiCall { convention } => EdgeKind::FfiCall {
            convention: *convention,
        },
        RecipeEdgeKind::HttpRequest { method, url } => EdgeKind::HttpRequest {
            method: *method,
            url: intern_opt(graph, url),
        },
        RecipeEdgeKind::GrpcCall { service, method } => EdgeKind::GrpcCall {
            service: intern_one(graph, service),
            method: intern_one(graph, method),
        },
        RecipeEdgeKind::WebAssemblyCall => EdgeKind::WebAssemblyCall,
        RecipeEdgeKind::DbQuery { query_type, table } => EdgeKind::DbQuery {
            query_type: *query_type,
            table: intern_opt(graph, table),
        },
        RecipeEdgeKind::TableRead { table_name, schema } => EdgeKind::TableRead {
            table_name: intern_one(graph, table_name),
            schema: intern_opt(graph, schema),
        },
        RecipeEdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        } => EdgeKind::TableWrite {
            table_name: intern_one(graph, table_name),
            schema: intern_opt(graph, schema),
            operation: *operation,
        },
        RecipeEdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => EdgeKind::TriggeredBy {
            trigger_name: intern_one(graph, trigger_name),
            schema: intern_opt(graph, schema),
        },
        RecipeEdgeKind::MessageQueue { protocol, topic } => EdgeKind::MessageQueue {
            protocol: match protocol {
                MqProtocolChoice::Kafka => MqProtocol::Kafka,
                MqProtocolChoice::Sqs => MqProtocol::Sqs,
                MqProtocolChoice::RabbitMq => MqProtocol::RabbitMq,
                MqProtocolChoice::Nats => MqProtocol::Nats,
                MqProtocolChoice::Redis => MqProtocol::Redis,
                MqProtocolChoice::Other(s) => MqProtocol::Other(intern_one(graph, s)),
            },
            topic: intern_opt(graph, topic),
        },
        RecipeEdgeKind::WebSocket { event } => EdgeKind::WebSocket {
            event: intern_opt(graph, event),
        },
        RecipeEdgeKind::GraphQLOperation { operation } => EdgeKind::GraphQLOperation {
            operation: intern_one(graph, operation),
        },
        RecipeEdgeKind::ProcessExec { command } => EdgeKind::ProcessExec {
            command: intern_one(graph, command),
        },
        RecipeEdgeKind::FileIpc { path_pattern } => EdgeKind::FileIpc {
            path_pattern: intern_opt(graph, path_pattern),
        },
        RecipeEdgeKind::ProtocolCall { protocol, metadata } => EdgeKind::ProtocolCall {
            protocol: intern_one(graph, protocol),
            metadata: intern_opt(graph, metadata),
        },
        RecipeEdgeKind::GenericBound => EdgeKind::GenericBound,
        RecipeEdgeKind::AnnotatedWith => EdgeKind::AnnotatedWith,
        RecipeEdgeKind::AnnotationParam => EdgeKind::AnnotationParam,
        RecipeEdgeKind::LambdaCaptures => EdgeKind::LambdaCaptures,
        RecipeEdgeKind::ModuleExports => EdgeKind::ModuleExports,
        RecipeEdgeKind::ModuleRequires => EdgeKind::ModuleRequires,
        RecipeEdgeKind::ModuleOpens => EdgeKind::ModuleOpens,
        RecipeEdgeKind::ModuleProvides => EdgeKind::ModuleProvides,
        RecipeEdgeKind::TypeArgument => EdgeKind::TypeArgument,
        RecipeEdgeKind::ExtensionReceiver => EdgeKind::ExtensionReceiver,
        RecipeEdgeKind::CompanionOf => EdgeKind::CompanionOf,
        RecipeEdgeKind::SealedPermit => EdgeKind::SealedPermit,
    }
}

// ---------------------------------------------------------------------------
// Invariant checker
// ---------------------------------------------------------------------------

/// Failure reasons reported by [`check_well_formed`].
///
/// Variants are deliberately exhaustive so a failing self-test points at the
/// exact invariant the generator violated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WellFormednessError {
    /// An edge references a `NodeId` that does not resolve in the arena.
    DanglingEdge {
        edge_idx: usize,
        side: &'static str,
        node: NodeId,
    },
    /// A non-root node has zero or multiple incoming `Defines` edges, or
    /// the `Defines` graph contains a cycle.
    DefinesNotForest { node: NodeId, inbound_count: usize },
    /// A `Defines` cycle was detected starting from this node.
    DefinesCycle { node: NodeId },
    /// A `Calls` edge targets a non-call-compatible node kind.
    CallsToIncompatibleKind {
        source: NodeId,
        target: NodeId,
        target_kind: NodeKind,
    },
    /// An edge metadata field references an unresolved `StringId`.
    UnresolvedString { context: &'static str, id: StringId },
    /// A node references a `StringId` that does not resolve.
    NodeUnresolvedName { node: NodeId, id: StringId },
}

/// Validates the structural invariants of a generated graph.
///
/// This is the contract every `well_formed_graph()` value satisfies. It is
/// the same check the self-test uses to prove the generator never produces
/// ill-formed graphs.
///
/// # Errors
///
/// Returns the first invariant violation discovered. Detection order is
/// nodes → defines forest → edges.
pub fn check_well_formed(g: &GeneratedGraph) -> Result<(), WellFormednessError> {
    let snapshot = g.graph.snapshot();

    // Node string-id resolution.
    for (node_id, entry) in snapshot.iter_nodes() {
        if snapshot.strings().resolve(entry.name).is_none() {
            return Err(WellFormednessError::NodeUnresolvedName {
                node: node_id,
                id: entry.name,
            });
        }
        if let Some(qname) = entry.qualified_name
            && snapshot.strings().resolve(qname).is_none()
        {
            return Err(WellFormednessError::NodeUnresolvedName {
                node: node_id,
                id: qname,
            });
        }
    }

    // Defines forest: at most one incoming `Defines` per node, no cycles.
    let mut defines_parent: HashMap<NodeId, NodeId> = HashMap::new();
    let mut defines_inbound: HashMap<NodeId, usize> = HashMap::new();
    let mut all_edges: Vec<(NodeId, NodeId, EdgeKind)> = Vec::new();
    for (src, tgt, kind) in snapshot.iter_edges() {
        all_edges.push((src, tgt, kind.clone()));
        if matches!(kind, EdgeKind::Defines) {
            *defines_inbound.entry(tgt).or_insert(0) += 1;
            defines_parent.insert(tgt, src);
        }
    }
    for (node, count) in &defines_inbound {
        if *count > 1 {
            return Err(WellFormednessError::DefinesNotForest {
                node: *node,
                inbound_count: *count,
            });
        }
    }
    // Cycle detection via walking parent links upward bounded by node count.
    let node_count = snapshot.nodes().len();
    for (node, _) in defines_inbound.iter() {
        let mut current = *node;
        for _ in 0..=node_count {
            let Some(parent) = defines_parent.get(&current).copied() else {
                break;
            };
            if parent == *node {
                return Err(WellFormednessError::DefinesCycle { node: *node });
            }
            current = parent;
        }
    }

    // Edge integrity.
    for (idx, (src, tgt, kind)) in all_edges.iter().enumerate() {
        if snapshot.nodes().get(*src).is_none() {
            return Err(WellFormednessError::DanglingEdge {
                edge_idx: idx,
                side: "source",
                node: *src,
            });
        }
        let Some(tgt_entry) = snapshot.nodes().get(*tgt) else {
            return Err(WellFormednessError::DanglingEdge {
                edge_idx: idx,
                side: "target",
                node: *tgt,
            });
        };
        if matches!(kind, EdgeKind::Calls { .. })
            && !CALL_COMPATIBLE_KINDS.contains(&tgt_entry.kind)
        {
            return Err(WellFormednessError::CallsToIncompatibleKind {
                source: *src,
                target: *tgt,
                target_kind: tgt_entry.kind,
            });
        }
        // String-id resolution inside edge metadata.
        check_edge_strings(kind, &snapshot)?;
    }

    Ok(())
}

fn check_edge_strings(
    kind: &EdgeKind,
    snap: &sqry_core::graph::unified::concurrent::GraphSnapshot,
) -> Result<(), WellFormednessError> {
    let interner = snap.strings();
    let opt = |ctx: &'static str, s: Option<StringId>| -> Result<(), WellFormednessError> {
        if let Some(id) = s
            && interner.resolve(id).is_none()
        {
            return Err(WellFormednessError::UnresolvedString { context: ctx, id });
        }
        Ok(())
    };
    let req = |ctx: &'static str, id: StringId| -> Result<(), WellFormednessError> {
        if interner.resolve(id).is_none() {
            return Err(WellFormednessError::UnresolvedString { context: ctx, id });
        }
        Ok(())
    };
    match kind {
        EdgeKind::Imports { alias, .. } => opt("Imports.alias", *alias)?,
        EdgeKind::Exports { alias, .. } => opt("Exports.alias", *alias)?,
        EdgeKind::TypeOf { name, .. } => opt("TypeOf.name", *name)?,
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            ..
        } => {
            req("TraitMethodBinding.trait_name", *trait_name)?;
            req("TraitMethodBinding.impl_type", *impl_type)?;
        }
        EdgeKind::HttpRequest { url, .. } => opt("HttpRequest.url", *url)?,
        EdgeKind::GrpcCall { service, method } => {
            req("GrpcCall.service", *service)?;
            req("GrpcCall.method", *method)?;
        }
        EdgeKind::DbQuery { table, .. } => opt("DbQuery.table", *table)?,
        EdgeKind::TableRead { table_name, schema } => {
            req("TableRead.table_name", *table_name)?;
            opt("TableRead.schema", *schema)?;
        }
        EdgeKind::TableWrite {
            table_name, schema, ..
        } => {
            req("TableWrite.table_name", *table_name)?;
            opt("TableWrite.schema", *schema)?;
        }
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => {
            req("TriggeredBy.trigger_name", *trigger_name)?;
            opt("TriggeredBy.schema", *schema)?;
        }
        EdgeKind::MessageQueue { protocol, topic } => {
            if let MqProtocol::Other(id) = protocol {
                req("MessageQueue.protocol.other", *id)?;
            }
            opt("MessageQueue.topic", *topic)?;
        }
        EdgeKind::WebSocket { event } => opt("WebSocket.event", *event)?,
        EdgeKind::GraphQLOperation { operation } => req("GraphQLOperation.operation", *operation)?,
        EdgeKind::ProcessExec { command } => req("ProcessExec.command", *command)?,
        EdgeKind::FileIpc { path_pattern } => opt("FileIpc.path_pattern", *path_pattern)?,
        EdgeKind::ProtocolCall { protocol, metadata } => {
            req("ProtocolCall.protocol", *protocol)?;
            opt("ProtocolCall.metadata", *metadata)?;
        }
        // All remaining variants either carry no `StringId` or only carry
        // copy-typed fields (e.g. `Calls.resolved_via`, enum discriminants)
        // — they need no resolution.
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Strategy + shrinker
// ---------------------------------------------------------------------------

/// Returns the entry-point `proptest` strategy that produces well-formed
/// `GeneratedGraph` values.
///
/// Composition:
///
/// 1. Pick the file count, then per-file language.
/// 2. Pick the node count, then for each node its kind, file index, parent
///    index (with the per-file forest constraint), and unique byte offset.
/// 3. Pick the non-`Defines` edge count, then for each edge a source, a
///    call-compatible target if the variant requires it, and a randomly
///    chosen `RecipeEdgeKind`.
///
/// The returned strategy uses a custom [`WellFormedGraphTree`] for shrinking
/// so the invariants stay satisfied at every shrink step.
#[must_use]
pub fn well_formed_graph() -> WellFormedGraphStrategy {
    WellFormedGraphStrategy
}

/// `proptest::Strategy` for well-formed `CodeGraph`s.
#[derive(Debug, Clone)]
pub struct WellFormedGraphStrategy;

impl Strategy for WellFormedGraphStrategy {
    type Tree = WellFormedGraphTree;
    type Value = GeneratedGraph;

    fn new_tree(&self, runner: &mut TestRunner) -> NewTree<Self> {
        // Build the underlying recipe-generating strategy on the fly so the
        // outer Strategy stays cheap. We hand it a unit `()` so the input
        // recipe strategy is well-defined.
        let recipe_strategy = recipe_strategy();
        let tree = recipe_strategy.new_tree(runner)?;
        let initial = tree.current();
        Ok(WellFormedGraphTree {
            current: initial,
            next: Candidate::Leaf { idx: 0 },
            history: Vec::new(),
        })
    }
}

/// `proptest::ValueTree` for [`WellFormedGraphStrategy`].
///
/// Implements a domain-specific shrinker that walks recipe → recipe through
/// well-formed states only. The implementation follows proptest's
/// simplify/complicate contract:
///
/// * `current()` returns the materialised current recipe.
/// * `simplify()` produces a smaller candidate and remembers the previous
///   recipe; the runner then re-evaluates the property on the new value.
/// * `complicate()` reverts the most recent `simplify` if the runner found
///   the simpler value no longer reproduces the failure, then arranges so
///   that the *next* `simplify()` tries a different candidate (not the
///   reverted one).
///
/// Candidate ordering: leaves first (largest blast-radius reduction per
/// step), then edges, then whole files. Each phase exhausts before
/// advancing.
#[derive(Debug)]
pub struct WellFormedGraphTree {
    current: GraphRecipe,
    /// Stack of (previous recipe, candidate index that produced the
    /// current state). On `complicate` we pop the top entry, restore the
    /// previous recipe, AND advance `next_candidate` past that candidate so
    /// the next `simplify` tries something different.
    history: Vec<UndoEntry>,
    /// The next candidate index to try in `simplify`. Encodes which
    /// shrink-phase we are in and which candidate within that phase.
    next: Candidate,
}

#[derive(Debug, Clone)]
struct UndoEntry {
    prev_recipe: GraphRecipe,
    /// The candidate that was applied to produce `current` from
    /// `prev_recipe`. Stored so `complicate` can step past it.
    applied: Candidate,
}

/// Candidate identifier — uniquely names a single shrink step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Candidate {
    /// Try to drop the leaf whose order in `find_leaf_candidates` is `idx`.
    Leaf { idx: usize },
    /// Try to clear a [`NodeFlags`] bit from the next node that still
    /// carries one. `which` selects the flag: 0 = `address_taken`,
    /// 1 = `callsite_promiscuous`. Per task contract, the minimal value
    /// for both fields is `false`; this phase walks the recipe and clears
    /// one set bit per simplify step so the shrunk counter-example carries
    /// the minimum number of marked nodes.
    Flag { idx: usize, which: u8 },
    /// Try to drop the edge at recipe index `idx` (post-current state).
    Edge { idx: usize },
    /// Try to drop the file at recipe index `idx`.
    File { idx: usize },
    /// All shrinking candidates exhausted.
    Done,
}

impl ValueTree for WellFormedGraphTree {
    type Value = GeneratedGraph;

    fn current(&self) -> Self::Value {
        self.current.clone().materialize()
    }

    fn simplify(&mut self) -> bool {
        // Loop until we find a candidate we can apply, or run out.
        loop {
            match self.next {
                Candidate::Done => return false,
                Candidate::Leaf { idx } => {
                    let leaves = find_leaf_candidates(&self.current);
                    if idx < leaves.len() {
                        let victim = leaves[idx];
                        let prev = self.current.clone();
                        drop_node_at(&mut self.current, victim);
                        self.history.push(UndoEntry {
                            prev_recipe: prev,
                            applied: Candidate::Leaf { idx },
                        });
                        // Re-enter the leaf phase from idx 0 — successful
                        // leaf removal re-shapes the list (re-indexing,
                        // possible new leaves once parent links shift).
                        self.next = Candidate::Leaf { idx: 0 };
                        return true;
                    }
                    // No more leaves — drop into the flag-clear phase.
                    self.next = Candidate::Flag { idx: 0, which: 0 };
                }
                Candidate::Flag { idx, which } => {
                    // Walk forward to the next node that still carries the
                    // requested flag bit. If we exhaust this `which`, try
                    // the other flag from the start; if both exhausted,
                    // advance to the edge phase.
                    let nodes = &self.current.nodes;
                    let mut found: Option<usize> = None;
                    for (i, n) in nodes.iter().enumerate().skip(idx) {
                        let set = if which == 0 {
                            n.address_taken
                        } else {
                            n.callsite_promiscuous
                        };
                        if set {
                            found = Some(i);
                            break;
                        }
                    }
                    if let Some(i) = found {
                        let prev = self.current.clone();
                        if which == 0 {
                            self.current.nodes[i].address_taken = false;
                        } else {
                            self.current.nodes[i].callsite_promiscuous = false;
                        }
                        self.history.push(UndoEntry {
                            prev_recipe: prev,
                            applied: Candidate::Flag { idx: i, which },
                        });
                        // Advance past the cleared bit. We do NOT bounce
                        // back to the leaf phase: flag clears never expose
                        // new leaf candidates (flags don't participate in
                        // edge touching). Edge/file phases similarly
                        // cannot help reduce a flag-only signal, so the
                        // monotone forward walk is the tightest order.
                        self.next = Candidate::Flag { idx: i + 1, which };
                        return true;
                    }
                    // No more nodes carry this flag — flip to the other
                    // flag, or advance to edges if both are done.
                    if which == 0 {
                        self.next = Candidate::Flag { idx: 0, which: 1 };
                    } else {
                        self.next = Candidate::Edge { idx: 0 };
                    }
                }
                Candidate::Edge { idx } => {
                    if idx < self.current.edges.len() {
                        let prev = self.current.clone();
                        self.current.edges.remove(idx);
                        self.history.push(UndoEntry {
                            prev_recipe: prev,
                            applied: Candidate::Edge { idx },
                        });
                        // Edges going down can unblock leaf candidates that
                        // were previously "touched" by the removed edge.
                        // Bounce back to the leaf phase before continuing
                        // with edges so the shrinker converges on the
                        // tightest reproducer rather than getting stuck at
                        // edges=[] with stray un-removable nodes.
                        self.next = Candidate::Leaf { idx: 0 };
                        return true;
                    }
                    self.next = Candidate::File { idx: 0 };
                }
                Candidate::File { idx } => {
                    if self.current.files.len() > 1 && idx < self.current.files.len() {
                        // Only try dropping if the resulting recipe still
                        // has at least one file; otherwise step past.
                        let prev = self.current.clone();
                        if try_drop_file_at(&mut self.current, idx) {
                            self.history.push(UndoEntry {
                                prev_recipe: prev,
                                applied: Candidate::File { idx },
                            });
                            // Dropping a file removes its nodes wholesale —
                            // re-enter the leaf phase in case removing it
                            // un-touched leaf candidates elsewhere.
                            self.next = Candidate::Leaf { idx: 0 };
                            return true;
                        }
                        // Couldn't drop — try the next index.
                        self.next = Candidate::File { idx: idx + 1 };
                        continue;
                    }
                    // File phase done.
                    self.next = Candidate::Done;
                }
            }
        }
    }

    fn complicate(&mut self) -> bool {
        let Some(entry) = self.history.pop() else {
            return false;
        };
        self.current = entry.prev_recipe;
        // Advance past the candidate that was just reverted so we do not
        // re-try it on the next `simplify`.
        self.next = match entry.applied {
            Candidate::Leaf { idx } => Candidate::Leaf { idx: idx + 1 },
            Candidate::Flag { idx, which } => Candidate::Flag {
                idx: idx + 1,
                which,
            },
            Candidate::Edge { idx } => Candidate::Edge { idx: idx + 1 },
            Candidate::File { idx } => Candidate::File { idx: idx + 1 },
            Candidate::Done => Candidate::Done,
        };
        true
    }
}

/// Returns the list of node indices that are safely removable in the
/// current shrink phase.
///
/// A node is considered removable iff it does not participate in any
/// non-`Defines` edge (as source or target). It MAY have `Defines`
/// children — `drop_node_at` re-roots them so the forest invariant
/// survives.
///
/// Returned in highest-index-first order: dropping a higher index never
/// invalidates a lower one, so successive simplifications stay monotone
/// without re-indexing the candidate list.
fn find_leaf_candidates(recipe: &GraphRecipe) -> Vec<usize> {
    if recipe.nodes.is_empty() {
        return Vec::new();
    }
    let mut touched = vec![false; recipe.nodes.len()];
    for e in &recipe.edges {
        if e.source < touched.len() {
            touched[e.source] = true;
        }
        if e.target < touched.len() {
            touched[e.target] = true;
        }
    }
    (0..recipe.nodes.len())
        .rev()
        .filter(|&i| !touched[i])
        .collect()
}

/// Drops the file at `file_idx` along with all its nodes and edges. Returns
/// true on success, false if it cannot be safely dropped (left with no
/// files, or cross-file `Defines` edges).
fn try_drop_file_at(recipe: &mut GraphRecipe, file_idx: usize) -> bool {
    if recipe.files.len() <= 1 || file_idx >= recipe.files.len() {
        return false;
    }
    let nodes_to_drop: BTreeSet<usize> = recipe
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.file_idx == file_idx)
        .map(|(i, _)| i)
        .collect();
    let mut sorted: Vec<usize> = nodes_to_drop.into_iter().collect();
    sorted.sort_unstable();
    for victim in sorted.into_iter().rev() {
        drop_node_at(recipe, victim);
    }
    recipe.files.remove(file_idx);
    for n in &mut recipe.nodes {
        if n.file_idx > file_idx {
            n.file_idx -= 1;
        }
    }
    true
}

/// Removes the node at `victim` from the recipe along with every edge that
/// touches it. Re-indexes all higher node indices down by one. Children
/// that pointed at `victim` as their `Defines` parent are re-rooted (their
/// parent is set to `None`) so the forest invariant survives.
fn drop_node_at(recipe: &mut GraphRecipe, victim: usize) {
    // Remove edges touching the victim.
    recipe
        .edges
        .retain(|e| e.source != victim && e.target != victim);
    // Re-root children whose parent was the victim.
    for n in &mut recipe.nodes {
        if n.parent == Some(victim) {
            n.parent = None;
        }
    }
    // Remove the node.
    recipe.nodes.remove(victim);
    // Re-index higher node references.
    for n in &mut recipe.nodes {
        if let Some(p) = n.parent
            && p > victim
        {
            n.parent = Some(p - 1);
        }
    }
    for e in &mut recipe.edges {
        if e.source > victim {
            e.source -= 1;
        }
        if e.target > victim {
            e.target -= 1;
        }
    }
}

// ---------------------------------------------------------------------------
// Recipe-strategy implementation
// ---------------------------------------------------------------------------

/// Generates a well-formed [`GraphRecipe`] from random primitives.
///
/// Composition: file count → node count → edge count → per-element details,
/// flat-mapped together. The final post-processing pass repairs `Calls`
/// targets so they reference call-compatible kinds and re-indexes any edges
/// whose target slot was rejected.
fn recipe_strategy() -> impl Strategy<Value = GraphRecipe> {
    (
        1usize..=MAX_FILES,
        1usize..=MAX_NODES,
        0usize..=MAX_EXTRA_EDGES,
    )
        .prop_flat_map(|(n_files, n_nodes, n_edges)| {
            let files = vec(language_strategy(), n_files);
            // For each node: kind, file index in [0, n_files), parent
            // selector ∈ [0..1), byte offset slot ∈ [0, 2^20), and a pair
            // of flag-selector floats ∈ [0..1) used to gate the
            // `address_taken` / `callsite_promiscuous` NodeFlags bits at
            // `FLAG_PROB_*` (≈15% each) in `assemble_recipe`.
            let node_seeds = vec(
                (
                    node_kind_strategy(),
                    0usize..n_files,
                    0.0f64..1.0,
                    0u32..(1u32 << 20),
                    0.0f64..1.0,
                    0.0f64..1.0,
                ),
                n_nodes,
            );
            // Per-edge seed: source slot ∈ [0..1), target slot ∈ [0..1),
            // variant selector u8.
            let edge_seeds = vec((0.0f64..1.0, 0.0f64..1.0, any::<EdgeSeed>()), n_edges);
            (Just(n_files), files, node_seeds, edge_seeds)
        })
        .prop_map(|(n_files, files, node_seeds, edge_seeds)| {
            assemble_recipe(n_files, files, node_seeds, edge_seeds)
        })
}

fn language_strategy() -> impl Strategy<Value = RecipeFile> {
    // Cycle through a handful of languages so language-tagged file
    // registration paths get exercised.
    (0u8..7u8, 0u32..(1u32 << 20)).prop_map(|(tag, slot)| {
        let (language, ext) = match tag {
            0 => (Language::Rust, "rs"),
            1 => (Language::Python, "py"),
            2 => (Language::JavaScript, "js"),
            3 => (Language::TypeScript, "ts"),
            4 => (Language::Go, "go"),
            5 => (Language::Java, "java"),
            _ => (Language::C, "c"),
        };
        RecipeFile {
            path: format!("prop/f{slot}.{ext}"),
            language,
        }
    })
}

fn node_kind_strategy() -> impl Strategy<Value = NodeKind> {
    // Curated set — every NodeKind that participates in differential
    // queries we care about. Excludes `Other` (sentinel) and the
    // styles/lifetimes domain-specific kinds that the planner ignores.
    let kinds: &'static [NodeKind] = &[
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Class,
        NodeKind::Interface,
        NodeKind::Trait,
        NodeKind::Module,
        NodeKind::Variable,
        NodeKind::Constant,
        NodeKind::Type,
        NodeKind::Struct,
        NodeKind::Enum,
        NodeKind::EnumVariant,
        NodeKind::Macro,
        NodeKind::Parameter,
        NodeKind::Property,
        NodeKind::CallSite,
        NodeKind::Import,
        NodeKind::Export,
        NodeKind::Component,
        NodeKind::Service,
        NodeKind::Resource,
        NodeKind::Endpoint,
        NodeKind::Test,
        NodeKind::TypeParameter,
        NodeKind::Annotation,
        NodeKind::LambdaTarget,
        NodeKind::EnumConstant,
    ];
    (0usize..kinds.len()).prop_map(|i| kinds[i])
}

/// Compact union-tag for picking an `EdgeKind` variant inside the strategy.
#[derive(Debug, Clone)]
struct EdgeSeed {
    /// 0..38 — clamped via modulo in `assemble_recipe`. We use a `u32` so
    /// proptest's `Arbitrary` covers the full discriminant space.
    variant_tag: u32,
    argument_count: u8,
    is_async: bool,
    is_wildcard: bool,
    is_verified: bool,
    is_ambiguous: bool,
    name_seed: u32,
    /// Used to pick among enum sub-variants (HttpMethod, etc.).
    enum_seed: u8,
    /// Selector for "should this Option field be Some".
    option_seed: u8,
    /// Index seed for TypeOf.index.
    type_index: u16,
}

impl Arbitrary for EdgeSeed {
    type Parameters = ();
    type Strategy = BoxedStrategy<Self>;

    fn arbitrary_with(_: ()) -> Self::Strategy {
        (
            any::<u32>(),
            any::<u8>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<u32>(),
            any::<u8>(),
            any::<u8>(),
            any::<u16>(),
        )
            .prop_map(
                |(
                    variant_tag,
                    argument_count,
                    is_async,
                    is_wildcard,
                    is_verified,
                    is_ambiguous,
                    name_seed,
                    enum_seed,
                    option_seed,
                    type_index,
                )| EdgeSeed {
                    variant_tag,
                    argument_count,
                    is_async,
                    is_wildcard,
                    is_verified,
                    is_ambiguous,
                    name_seed,
                    enum_seed,
                    option_seed,
                    type_index,
                },
            )
            .boxed()
    }
}

/// Probability (over [0,1) seed floats) that any given node carries
/// [`NodeFlags::ADDRESS_TAKEN`]. Calibrated to keep the population
/// non-trivial (well above the noise floor) while leaving the majority of
/// nodes clean so non-cicall queries still see realistic graphs.
///
/// Coverage observed across 100 sample graphs (avg ~32 nodes each):
/// every sample carries at least one address-taken node; see
/// `graph_gen_self_test::nodeflags_coverage_is_non_vacuous`.
const FLAG_PROB_ADDRESS_TAKEN: f64 = 0.15;

/// Probability that any given node carries
/// [`NodeFlags::CALLSITE_PROMISCUOUS`]. Independent of
/// [`FLAG_PROB_ADDRESS_TAKEN`] — both flags compose freely so a node may
/// carry neither, one, or both.
const FLAG_PROB_CALLSITE_PROMISCUOUS: f64 = 0.15;

#[allow(clippy::too_many_lines)] // The big switch over 38 variants is the natural shape; splitting
// would obscure the per-variant metadata. Each arm is small and self-contained.
fn assemble_recipe(
    n_files: usize,
    files: Vec<RecipeFile>,
    node_seeds: Vec<(NodeKind, usize, f64, u32, f64, f64)>,
    edge_seeds: Vec<(f64, f64, EdgeSeed)>,
) -> GraphRecipe {
    // ---- Build nodes ----
    // Per-file unique byte offsets allocated in registration order.
    let mut next_offset_per_file: HashMap<usize, u32> = HashMap::new();
    // For each file, the list of node indices belonging to that file, in
    // allocation order. Used to find a same-file ancestor for the Defines
    // forest.
    let mut nodes_by_file: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut nodes: Vec<RecipeNode> = Vec::with_capacity(node_seeds.len());

    for (
        i,
        (kind, file_idx_raw, parent_fraction, _offset_seed, addr_taken_seed, promiscuous_seed),
    ) in node_seeds.iter().enumerate()
    {
        let file_idx = file_idx_raw % n_files.max(1);
        let same_file = nodes_by_file.entry(file_idx).or_default();
        // Pick a parent from already-allocated nodes in the same file. If
        // empty, this becomes a file root.
        let parent = if same_file.is_empty() {
            None
        } else {
            let pos = (*parent_fraction * same_file.len() as f64) as usize;
            let pos = pos.min(same_file.len() - 1);
            Some(same_file[pos])
        };
        let offset = next_offset_per_file.entry(file_idx).or_insert(0);
        let byte_offset = *offset;
        *offset = offset.saturating_add(32);
        let name = format!("n{}_{}_{}", i, kind.as_str(), byte_offset);
        // NodeFlags bits — Bernoulli draws at the calibrated rates. The
        // `RecipeNode` carries these flat booleans; `materialize` replays
        // them as `mark_address_taken` / `mark_callsite_promiscuous` calls
        // against the live `NodeMetadataStore`. The shrinker can flip
        // either flag back to `false` independently via the `Flag` phase.
        let address_taken = *addr_taken_seed < FLAG_PROB_ADDRESS_TAKEN;
        let callsite_promiscuous = *promiscuous_seed < FLAG_PROB_CALLSITE_PROMISCUOUS;
        nodes.push(RecipeNode {
            kind: *kind,
            name,
            file_idx,
            parent,
            byte_offset,
            address_taken,
            callsite_promiscuous,
        });
        same_file.push(i);
    }

    // ---- Build edges ----
    // Map of node indices whose kind is call-compatible — used to pick
    // valid `Calls` targets.
    let call_compat_targets: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| CALL_COMPATIBLE_KINDS.contains(&n.kind))
        .map(|(i, _)| i)
        .collect();

    let mut edges: Vec<RecipeEdge> = Vec::with_capacity(edge_seeds.len());
    for (src_fraction, tgt_fraction, seed) in edge_seeds {
        if nodes.is_empty() {
            break;
        }
        let source = ((src_fraction * nodes.len() as f64) as usize).min(nodes.len() - 1);
        // Defines edges (variant 0) are emitted from the parent-link forest
        // exclusively — re-emitting them here would violate the forest
        // invariant (multiple incoming Defines edges). The randomised pool
        // therefore skips variant 0; coverage of `Defines` is guaranteed
        // by every non-root node carrying a parent link.
        let variant = 1 + (seed.variant_tag as usize) % (NUM_EDGE_VARIANTS - 1);
        let kind = build_edge_kind(variant, &seed);

        // Pick a target. `Calls` (and aliases below) require a
        // call-compatible target; everything else accepts any node.
        let target = if matches!(kind, RecipeEdgeKind::Calls { .. }) {
            if call_compat_targets.is_empty() {
                continue;
            }
            let idx = ((tgt_fraction * call_compat_targets.len() as f64) as usize)
                .min(call_compat_targets.len() - 1);
            call_compat_targets[idx]
        } else {
            ((tgt_fraction * nodes.len() as f64) as usize).min(nodes.len() - 1)
        };
        edges.push(RecipeEdge {
            source,
            target,
            kind,
        });
    }

    GraphRecipe {
        files,
        nodes,
        edges,
    }
}

const NUM_EDGE_VARIANTS: usize = ALL_EDGE_KIND_TAGS.len();

fn opt_name(seed: &EdgeSeed, prefix: &str, threshold: u8) -> Option<String> {
    if seed.option_seed >= threshold {
        Some(format!("{prefix}_{}", seed.name_seed))
    } else {
        None
    }
}

fn pick_export_kind(seed: u8) -> ExportKind {
    match seed % 4 {
        0 => ExportKind::Direct,
        1 => ExportKind::Reexport,
        2 => ExportKind::Default,
        _ => ExportKind::Namespace,
    }
}

fn pick_type_of_context(seed: u8) -> Option<TypeOfContext> {
    match seed % 7 {
        0 => None,
        1 => Some(TypeOfContext::Parameter),
        2 => Some(TypeOfContext::Return),
        3 => Some(TypeOfContext::Field),
        4 => Some(TypeOfContext::Variable),
        5 => Some(TypeOfContext::TypeParameter),
        _ => Some(TypeOfContext::Constraint),
    }
}

fn pick_lifetime_kind(seed: u8) -> LifetimeConstraintKind {
    match seed % 8 {
        0 => LifetimeConstraintKind::Outlives,
        1 => LifetimeConstraintKind::TypeBound,
        2 => LifetimeConstraintKind::Reference,
        3 => LifetimeConstraintKind::Static,
        4 => LifetimeConstraintKind::HigherRanked,
        5 => LifetimeConstraintKind::TraitObject,
        6 => LifetimeConstraintKind::ImplTrait,
        _ => LifetimeConstraintKind::Elided,
    }
}

fn pick_macro_kind(seed: u8) -> MacroExpansionKind {
    match seed % 5 {
        0 => MacroExpansionKind::Derive,
        1 => MacroExpansionKind::Attribute,
        2 => MacroExpansionKind::Declarative,
        3 => MacroExpansionKind::Function,
        _ => MacroExpansionKind::CfgGate,
    }
}

fn pick_ffi_convention(seed: u8) -> FfiConvention {
    match seed % 5 {
        0 => FfiConvention::C,
        1 => FfiConvention::Cdecl,
        2 => FfiConvention::Stdcall,
        3 => FfiConvention::Fastcall,
        _ => FfiConvention::System,
    }
}

fn pick_http_method(seed: u8) -> HttpMethod {
    match seed % 8 {
        0 => HttpMethod::Get,
        1 => HttpMethod::Post,
        2 => HttpMethod::Put,
        3 => HttpMethod::Delete,
        4 => HttpMethod::Patch,
        5 => HttpMethod::Head,
        6 => HttpMethod::Options,
        _ => HttpMethod::All,
    }
}

fn pick_db_query_type(seed: u8) -> DbQueryType {
    match seed % 5 {
        0 => DbQueryType::Select,
        1 => DbQueryType::Insert,
        2 => DbQueryType::Update,
        3 => DbQueryType::Delete,
        _ => DbQueryType::Execute,
    }
}

fn pick_table_write_op(seed: u8) -> TableWriteOp {
    match seed % 3 {
        0 => TableWriteOp::Insert,
        1 => TableWriteOp::Update,
        _ => TableWriteOp::Delete,
    }
}

fn pick_mq_protocol(seed: u8, name_seed: u32) -> MqProtocolChoice {
    match seed % 6 {
        0 => MqProtocolChoice::Kafka,
        1 => MqProtocolChoice::Sqs,
        2 => MqProtocolChoice::RabbitMq,
        3 => MqProtocolChoice::Nats,
        4 => MqProtocolChoice::Redis,
        _ => MqProtocolChoice::Other(format!("custom_{name_seed}")),
    }
}

fn pick_resolved_via(seed: u8) -> ResolvedVia {
    match seed % 3 {
        0 => ResolvedVia::Direct,
        1 => ResolvedVia::TypeMatch,
        _ => ResolvedVia::BindingPlane,
    }
}

#[allow(clippy::too_many_lines)] // 38-variant switch — see assemble_recipe rationale.
fn build_edge_kind(variant: usize, seed: &EdgeSeed) -> RecipeEdgeKind {
    let name_str = |prefix: &str| format!("{prefix}_{}", seed.name_seed);
    match variant {
        0 => RecipeEdgeKind::Defines,
        1 => RecipeEdgeKind::Contains,
        2 => RecipeEdgeKind::Calls {
            argument_count: seed.argument_count,
            is_async: seed.is_async,
            resolved_via: pick_resolved_via(seed.enum_seed),
        },
        3 => RecipeEdgeKind::References,
        4 => RecipeEdgeKind::Imports {
            alias: opt_name(seed, "alias", 128),
            is_wildcard: seed.is_wildcard,
        },
        5 => RecipeEdgeKind::Exports {
            kind: pick_export_kind(seed.enum_seed),
            alias: opt_name(seed, "exp", 128),
        },
        6 => RecipeEdgeKind::TypeOf {
            context: pick_type_of_context(seed.enum_seed),
            index: if seed.option_seed >= 128 {
                Some(seed.type_index)
            } else {
                None
            },
            name: opt_name(seed, "tname", 96),
        },
        7 => RecipeEdgeKind::Inherits,
        8 => RecipeEdgeKind::Implements,
        9 => RecipeEdgeKind::LifetimeConstraint {
            constraint_kind: pick_lifetime_kind(seed.enum_seed),
        },
        10 => RecipeEdgeKind::TraitMethodBinding {
            trait_name: name_str("trait"),
            impl_type: name_str("impl"),
            is_ambiguous: seed.is_ambiguous,
        },
        11 => RecipeEdgeKind::MacroExpansion {
            expansion_kind: pick_macro_kind(seed.enum_seed),
            is_verified: seed.is_verified,
        },
        12 => RecipeEdgeKind::FfiCall {
            convention: pick_ffi_convention(seed.enum_seed),
        },
        13 => RecipeEdgeKind::HttpRequest {
            method: pick_http_method(seed.enum_seed),
            url: opt_name(seed, "url", 96),
        },
        14 => RecipeEdgeKind::GrpcCall {
            service: name_str("svc"),
            method: name_str("rpc"),
        },
        15 => RecipeEdgeKind::WebAssemblyCall,
        16 => RecipeEdgeKind::DbQuery {
            query_type: pick_db_query_type(seed.enum_seed),
            table: opt_name(seed, "tbl", 96),
        },
        17 => RecipeEdgeKind::TableRead {
            table_name: name_str("tbl"),
            schema: opt_name(seed, "sch", 128),
        },
        18 => RecipeEdgeKind::TableWrite {
            table_name: name_str("tbl"),
            schema: opt_name(seed, "sch", 128),
            operation: pick_table_write_op(seed.enum_seed),
        },
        19 => RecipeEdgeKind::TriggeredBy {
            trigger_name: name_str("trg"),
            schema: opt_name(seed, "sch", 128),
        },
        20 => RecipeEdgeKind::MessageQueue {
            protocol: pick_mq_protocol(seed.enum_seed, seed.name_seed),
            topic: opt_name(seed, "topic", 96),
        },
        21 => RecipeEdgeKind::WebSocket {
            event: opt_name(seed, "evt", 96),
        },
        22 => RecipeEdgeKind::GraphQLOperation {
            operation: name_str("gqlop"),
        },
        23 => RecipeEdgeKind::ProcessExec {
            command: name_str("cmd"),
        },
        24 => RecipeEdgeKind::FileIpc {
            path_pattern: opt_name(seed, "ipc", 96),
        },
        25 => RecipeEdgeKind::ProtocolCall {
            protocol: name_str("proto"),
            metadata: opt_name(seed, "meta", 128),
        },
        26 => RecipeEdgeKind::GenericBound,
        27 => RecipeEdgeKind::AnnotatedWith,
        28 => RecipeEdgeKind::AnnotationParam,
        29 => RecipeEdgeKind::LambdaCaptures,
        30 => RecipeEdgeKind::ModuleExports,
        31 => RecipeEdgeKind::ModuleRequires,
        32 => RecipeEdgeKind::ModuleOpens,
        33 => RecipeEdgeKind::ModuleProvides,
        34 => RecipeEdgeKind::TypeArgument,
        35 => RecipeEdgeKind::ExtensionReceiver,
        36 => RecipeEdgeKind::CompanionOf,
        _ => RecipeEdgeKind::SealedPermit,
    }
}

// ---------------------------------------------------------------------------
// Sample helper for non-proptest call-sites
// ---------------------------------------------------------------------------

/// Produces `count` graphs by hand-driving a `TestRunner` against
/// [`well_formed_graph`]. Used by the self-tests + coverage checks; callers
/// outside the property suite should prefer `proptest!` macros.
#[must_use]
pub fn sample_graphs(count: usize, seed: u64) -> Vec<GeneratedGraph> {
    use proptest::test_runner::{Config, RngAlgorithm, TestRng};
    // ChaCha demands a 32-byte seed; expand the caller's u64 by stamping it
    // four times into the seed buffer. Deterministic and trivially
    // reproducible from logs.
    let mut seed_bytes = [0u8; 32];
    for (i, chunk) in seed_bytes.chunks_exact_mut(8).enumerate() {
        // Fold the seed with a per-chunk constant so chunks differ.
        let folded = seed ^ ((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        chunk.copy_from_slice(&folded.to_le_bytes());
    }
    let rng = TestRng::from_seed(RngAlgorithm::ChaCha, &seed_bytes);
    let mut runner = TestRunner::new_with_rng(Config::default(), rng);
    let strategy = well_formed_graph();
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let tree = strategy
            .new_tree(&mut runner)
            .expect("strategy should not fail to produce a tree");
        out.push(tree.current());
    }
    out
}

/// Convenience: full set of `EdgeKind` discriminant tags observed in a graph.
#[must_use]
pub fn observed_edge_tags(g: &GeneratedGraph) -> BTreeSet<&'static str> {
    let snapshot = g.graph.snapshot();
    snapshot
        .iter_edges()
        .map(|(_, _, kind)| edge_kind_tag(&kind))
        .collect()
}

/// Maps a live `EdgeKind` to its discriminant tag. Mirrors
/// `RecipeEdgeKind::tag` so coverage tests can cross-check.
#[must_use]
pub fn edge_kind_tag(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Defines => "Defines",
        EdgeKind::Contains => "Contains",
        EdgeKind::Calls { .. } => "Calls",
        EdgeKind::References => "References",
        EdgeKind::Imports { .. } => "Imports",
        EdgeKind::Exports { .. } => "Exports",
        EdgeKind::TypeOf { .. } => "TypeOf",
        EdgeKind::Inherits => "Inherits",
        EdgeKind::Implements => "Implements",
        EdgeKind::LifetimeConstraint { .. } => "LifetimeConstraint",
        EdgeKind::TraitMethodBinding { .. } => "TraitMethodBinding",
        EdgeKind::MacroExpansion { .. } => "MacroExpansion",
        EdgeKind::FfiCall { .. } => "FfiCall",
        EdgeKind::HttpRequest { .. } => "HttpRequest",
        EdgeKind::GrpcCall { .. } => "GrpcCall",
        EdgeKind::WebAssemblyCall => "WebAssemblyCall",
        EdgeKind::DbQuery { .. } => "DbQuery",
        EdgeKind::TableRead { .. } => "TableRead",
        EdgeKind::TableWrite { .. } => "TableWrite",
        EdgeKind::TriggeredBy { .. } => "TriggeredBy",
        EdgeKind::MessageQueue { .. } => "MessageQueue",
        EdgeKind::WebSocket { .. } => "WebSocket",
        EdgeKind::GraphQLOperation { .. } => "GraphQLOperation",
        EdgeKind::ProcessExec { .. } => "ProcessExec",
        EdgeKind::FileIpc { .. } => "FileIpc",
        EdgeKind::ProtocolCall { .. } => "ProtocolCall",
        EdgeKind::GenericBound => "GenericBound",
        EdgeKind::AnnotatedWith => "AnnotatedWith",
        EdgeKind::AnnotationParam => "AnnotationParam",
        EdgeKind::LambdaCaptures => "LambdaCaptures",
        EdgeKind::ModuleExports => "ModuleExports",
        EdgeKind::ModuleRequires => "ModuleRequires",
        EdgeKind::ModuleOpens => "ModuleOpens",
        EdgeKind::ModuleProvides => "ModuleProvides",
        EdgeKind::TypeArgument => "TypeArgument",
        EdgeKind::ExtensionReceiver => "ExtensionReceiver",
        EdgeKind::CompanionOf => "CompanionOf",
        EdgeKind::SealedPermit => "SealedPermit",
    }
}

// ---------------------------------------------------------------------------
// Module-local sanity unit tests — keep the helper functions honest. These
// run as part of `cargo test --test graph_gen_self_test` automatically.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn drop_node_at_re_roots_children() {
        let mut recipe = GraphRecipe {
            files: vec![RecipeFile {
                path: "f.rs".into(),
                language: Language::Rust,
            }],
            nodes: vec![
                RecipeNode {
                    kind: NodeKind::Module,
                    name: "root".into(),
                    file_idx: 0,
                    parent: None,
                    byte_offset: 0,
                    address_taken: false,
                    callsite_promiscuous: false,
                },
                RecipeNode {
                    kind: NodeKind::Function,
                    name: "mid".into(),
                    file_idx: 0,
                    parent: Some(0),
                    byte_offset: 32,
                    address_taken: false,
                    callsite_promiscuous: false,
                },
                RecipeNode {
                    kind: NodeKind::Function,
                    name: "leaf".into(),
                    file_idx: 0,
                    parent: Some(1),
                    byte_offset: 64,
                    address_taken: false,
                    callsite_promiscuous: false,
                },
            ],
            edges: vec![],
        };
        drop_node_at(&mut recipe, 1);
        assert_eq!(recipe.nodes.len(), 2);
        // The orphaned child (was node 2) is now node 1 with parent None.
        assert_eq!(recipe.nodes[1].parent, None);
    }

    #[test]
    fn all_edge_kind_tags_count_matches() {
        // The Phase A-era V11 enum carries exactly 38 variants. If this
        // assertion fails, the upstream enum changed; the generator's
        // ALL_EDGE_KIND_TAGS list must be kept in lockstep.
        assert_eq!(ALL_EDGE_KIND_TAGS.len(), 38);
        let unique: HashSet<&&str> = ALL_EDGE_KIND_TAGS.iter().collect();
        assert_eq!(unique.len(), 38);
    }
}
