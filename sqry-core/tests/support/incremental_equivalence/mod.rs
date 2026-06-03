//! Semantic-equivalence harness for the incremental rebuild engine.
//!
//! This module defines the §E property-based equivalence contract from the
//! sqryd daemon design: the graph returned by
//! [`incremental_rebuild`][ir] must be **semantically equivalent** to the
//! graph a full rebuild of the same filesystem state produces.
//!
//! "Semantically equivalent" explicitly ignores artifacts of the allocation
//! order (raw [`NodeId`], [`EdgeId`], arena slots, CSR offsets, [`StringId`]
//! values) because those differ between fresh builds by construction. What
//! the harness compares instead is the semantic surface:
//!
//! - Every node is keyed by (file path relative to the workspace root,
//!   [`NodeKind`], qualified name, signature hash, span byte range).
//! - Every edge is keyed by (source [`NodeSemKey`], target [`NodeSemKey`],
//!   canonical edge kind — with all [`StringId`] payloads resolved to
//!   strings — plus a span-set discriminator that distinguishes multiple
//!   edges between the same two nodes).
//!
//! [ir]: sqry_core::graph::unified::build::incremental::incremental_rebuild
//! [`NodeId`]: sqry_core::graph::unified::node::NodeId
//! [`EdgeId`]: sqry_core::graph::unified::edge::EdgeId
//! [`StringId`]: sqry_core::graph::unified::StringId
//! [`NodeKind`]: sqry_core::graph::unified::node::NodeKind

#![allow(dead_code)] // Harness utilities — some exports only used from the
// proptest binary, which is a separate integration target.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::kind::{
    ChannelBufferKind, ChannelPeerDirection, DbQueryType, EdgeKind, ExportKind, FfiConvention,
    HttpMethod, InferenceKind, LifetimeConstraintKind, MacroExpansionKind, MqProtocol,
    TableWriteOp, TypeOfContext, WrapKind,
};
use sqry_core::graph::unified::node::NodeKind;

/// Stable semantic identity for a node, deliberately omitting any data that
/// depends on allocation order (raw NodeId, arena slot, generation).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeSemKey {
    /// File path RELATIVE to the workspace root, with forward-slash
    /// separators. Relative paths make the key stable across tempdir moves.
    pub file_path: Arc<str>,
    /// Kind of entity.
    pub kind: NodeKind,
    /// Module-qualified symbol path. Falls back to the unqualified name
    /// (then the empty string) when neither is present so nodes without a
    /// qualified name still compare by their plain name.
    pub qualified_name: Arc<str>,
    /// Deterministic hash of the signature/type string, or 0 when absent.
    /// We hash rather than keep the full string so the key stays small;
    /// collisions would merely create stable false-positives that still
    /// compare identically across builds.
    pub signature_hash: u64,
    /// Start byte offset in the source file.
    pub span_byte_start: u32,
    /// End byte offset in the source file.
    pub span_byte_end: u32,
}

/// Stable semantic identity for an edge, deliberately omitting EdgeId and
/// CSR offset. Uses a hash-set container since not every edge-kind payload
/// (e.g., [`HttpMethod`], [`FfiConvention`]) implements [`Ord`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EdgeSemKey {
    /// Source node key.
    pub source: NodeSemKey,
    /// Target node key.
    pub target: NodeSemKey,
    /// Canonicalized edge kind with all StringId payloads resolved to
    /// strings.
    pub kind: CanonicalEdgeKind,
    /// Discriminator that distinguishes multiple edges between the same two
    /// nodes. Derived from the sorted span list of the edge so edges at
    /// different call sites compare as distinct even when they have the
    /// same kind and endpoints.
    pub span_discriminator: u64,
}

/// An [`EdgeKind`] with every [`StringId`] payload resolved to a stable
/// [`Arc<str>`]. Mirrors every variant of `EdgeKind` 1:1 so metadata is
/// preserved for the equivalence check.
///
/// Two graphs using different string interners (different `StringId`
/// values for the same underlying name) still produce identical
/// `CanonicalEdgeKind` values when their canonical names match.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)] // Mirrors the public EdgeKind vocabulary.
pub enum CanonicalEdgeKind {
    Defines,
    Contains,
    Calls {
        argument_count: u8,
        is_async: bool,
    },
    References,
    Imports {
        alias: Option<Arc<str>>,
        is_wildcard: bool,
    },
    Exports {
        kind: ExportKind,
        alias: Option<Arc<str>>,
    },
    TypeOf {
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<Arc<str>>,
    },
    Inherits,
    Implements,
    LifetimeConstraint {
        constraint_kind: LifetimeConstraintKind,
    },
    TraitMethodBinding {
        trait_name: Arc<str>,
        impl_type: Arc<str>,
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
        url: Option<Arc<str>>,
    },
    GrpcCall {
        service: Arc<str>,
        method: Arc<str>,
    },
    WebAssemblyCall,
    DbQuery {
        query_type: DbQueryType,
        table: Option<Arc<str>>,
    },
    TableRead {
        table_name: Arc<str>,
        schema: Option<Arc<str>>,
    },
    TableWrite {
        table_name: Arc<str>,
        schema: Option<Arc<str>>,
        operation: TableWriteOp,
    },
    TriggeredBy {
        trigger_name: Arc<str>,
        schema: Option<Arc<str>>,
    },
    MessageQueue {
        protocol: CanonicalMqProtocol,
        topic: Option<Arc<str>>,
    },
    WebSocket {
        event: Option<Arc<str>>,
    },
    GraphQLOperation {
        operation: Arc<str>,
    },
    ProcessExec {
        command: Arc<str>,
    },
    FileIpc {
        path_pattern: Option<Arc<str>>,
    },
    ProtocolCall {
        protocol: Arc<str>,
        metadata: Option<Arc<str>>,
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
    Wraps {
        kind: WrapKind,
        chain_position: Option<u16>,
    },
    ChannelPeer {
        direction: ChannelPeerDirection,
        buffer_kind: ChannelBufferKind,
    },
    Instantiates {
        /// Resolved `(type-name, default_typed)` slots in declaration order.
        type_args: Vec<(Arc<str>, bool)>,
        inference_kind: InferenceKind,
    },
}

/// Canonical form of [`MqProtocol`] that resolves the `Other(StringId)`
/// payload to a stable string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CanonicalMqProtocol {
    Kafka,
    Sqs,
    RabbitMq,
    Nats,
    Redis,
    Other(Arc<str>),
}

/// Canonicalized representation of a whole graph — the only object the
/// equivalence asserter actually compares. Node keys live in a
/// [`BTreeMap`] so diff output is deterministic; edge sets use
/// [`HashSet`] since not every edge-kind payload is `Ord`.
pub type SemGraph = BTreeMap<NodeSemKey, HashSet<EdgeSemKey>>;

/// Build a [`SemGraph`] from a [`CodeGraph`] by walking every occupied
/// node in the arena, resolving its outgoing edges, and converting both
/// sides into their semantic-key form.
///
/// `workspace_root` is used to make file paths relative so the same
/// logical graph built in two different tempdirs compares equal. Paths
/// that fail to strip the prefix (e.g., external files outside the
/// workspace) fall back to their full canonical path, keeping them
/// comparable across builds that share the same absolute layout.
#[must_use]
pub fn build_sem_graph(graph: &CodeGraph, workspace_root: &Path) -> SemGraph {
    let mut out: SemGraph = BTreeMap::new();

    // First pass: build the node-id → NodeSemKey table. We need it to be
    // able to translate edge endpoints in the second pass.
    let mut id_to_key: std::collections::HashMap<
        sqry_core::graph::unified::node::NodeId,
        NodeSemKey,
    > = std::collections::HashMap::new();

    for (node_id, entry) in graph.nodes().iter() {
        let key = make_node_sem_key(graph, workspace_root, entry);
        id_to_key.insert(node_id, key.clone());
        out.entry(key).or_default();
    }

    // Second pass: resolve edges. For each node, enumerate outgoing edges,
    // translate both endpoints to their NodeSemKeys, and build EdgeSemKeys.
    // Edges whose target points to a tombstoned node (not present in the
    // arena) are silently skipped — they are a known consequence of the
    // unified graph's lazy compaction and should not surface in the
    // semantic view.
    for (source_id, _source_entry) in graph.nodes().iter() {
        let Some(source_key) = id_to_key.get(&source_id) else {
            continue;
        };
        for edge in graph.edges().edges_from(source_id) {
            let Some(target_key) = id_to_key.get(&edge.target) else {
                continue;
            };
            let canonical_kind = canonicalize_edge_kind(graph, &edge.kind);
            let span_discriminator = span_set_hash(&edge.spans);
            let edge_key = EdgeSemKey {
                source: source_key.clone(),
                target: target_key.clone(),
                kind: canonical_kind,
                span_discriminator,
            };
            out.entry(source_key.clone()).or_default().insert(edge_key);
        }
    }

    out
}

/// Build a [`NodeSemKey`] from a [`NodeEntry`].
fn make_node_sem_key(
    graph: &CodeGraph,
    workspace_root: &Path,
    entry: &sqry_core::graph::unified::storage::NodeEntry,
) -> NodeSemKey {
    let file_path = canonical_file_path(graph, workspace_root, entry.file);
    let qualified_name_raw = resolve_qualified_name(graph, entry);
    let qualified_name = canonicalize_qualified_name(&qualified_name_raw, workspace_root);
    let signature_hash = entry
        .signature
        .and_then(|id| graph.strings().resolve(id))
        .map_or(0, |s| {
            deterministic_hash(canonicalize_signature_like(s.as_ref(), workspace_root).as_bytes())
        });

    NodeSemKey {
        file_path,
        kind: entry.kind,
        qualified_name,
        signature_hash,
        span_byte_start: entry.start_byte,
        span_byte_end: entry.end_byte,
    }
}

/// Strip any occurrence of the absolute `workspace_root` path from a
/// qualified name and normalise slashes. Some plugins (notably Python and
/// JavaScript) embed the file's absolute path directly in the qualified
/// name to disambiguate module-scoped imports; that absolute path differs
/// between the baseline and incremental tempdirs and would otherwise make
/// identical logical nodes appear distinct.
fn canonicalize_qualified_name(raw: &Arc<str>, workspace_root: &Path) -> Arc<str> {
    let canonical = canonicalize_signature_like(raw, workspace_root);
    Arc::from(canonical)
}

/// Shared rewrite used by both qualified-name and signature string
/// canonicalisation. Strips the workspace-root prefix anywhere it appears
/// (as a prefix, inside quotes, etc.) and normalises path separators.
fn canonicalize_signature_like(s: &str, workspace_root: &Path) -> String {
    let root_str = workspace_root.to_string_lossy();
    if root_str.is_empty() {
        return s.replace('\\', "/");
    }
    // Replace the absolute root with a stable sentinel so the rest of the
    // qualified name (module path relative to the root) stays stable.
    let with_sentinel = s.replace(root_str.as_ref(), "<WORKSPACE_ROOT>");
    with_sentinel.replace('\\', "/")
}

/// Canonicalize a file path: absolute → relative-to-workspace-root with
/// forward slashes. Falls back to the absolute path when the file is not
/// under the workspace root.
fn canonical_file_path(
    graph: &CodeGraph,
    workspace_root: &Path,
    file_id: sqry_core::graph::unified::file::FileId,
) -> Arc<str> {
    let Some(full_path) = graph.files().resolve(file_id) else {
        return Arc::from("<unresolved>");
    };

    let relative = full_path
        .strip_prefix(workspace_root)
        .unwrap_or(full_path.as_ref());

    let as_string = relative.to_string_lossy();
    // Use forward slashes unconditionally so Windows and Unix builds
    // compare equal.
    let normalized = as_string.replace('\\', "/");
    Arc::from(normalized)
}

/// Resolve the qualified name (or fall back to the bare name).
fn resolve_qualified_name(
    graph: &CodeGraph,
    entry: &sqry_core::graph::unified::storage::NodeEntry,
) -> Arc<str> {
    if let Some(qn_id) = entry.qualified_name
        && let Some(s) = graph.strings().resolve(qn_id)
    {
        return Arc::from(s.as_ref());
    }
    if let Some(s) = graph.strings().resolve(entry.name) {
        return Arc::from(s.as_ref());
    }
    Arc::from("")
}

/// Deterministic hash using `std::hash::DefaultHasher::new()` which — as
/// of Rust 1.94 — is a `SipHasher13` seeded with zero keys. Re-running
/// the harness in the same Rust toolchain produces stable values.
fn deterministic_hash(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn span_set_hash(spans: &[sqry_core::graph::node::Span]) -> u64 {
    // Sort spans by (start_line, start_column, end_line, end_column) and
    // hash the resulting sequence. Sorting ensures two semantically-
    // equivalent span sets hash identically even if they were collected in
    // different orders across the two builds.
    //
    // `graph::node::Span` is line/column-based rather than byte-based, so
    // we cast the usize fields through `u64` (widen lossless) for a stable
    // in-memory representation before hashing.
    let mut tuples: Vec<(u64, u64, u64, u64)> = spans
        .iter()
        .map(|span| {
            (
                span.start.line as u64,
                span.start.column as u64,
                span.end.line as u64,
                span.end.column as u64,
            )
        })
        .collect();
    tuples.sort_unstable();

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    tuples.hash(&mut hasher);
    hasher.finish()
}

/// Resolve an Option<StringId> to an Option<Arc<str>> via the graph's
/// interner. Missing entries (unresolvable StringIds) collapse to None.
fn resolve_opt_string(
    graph: &CodeGraph,
    id: Option<sqry_core::graph::unified::StringId>,
) -> Option<Arc<str>> {
    id.and_then(|sid| graph.strings().resolve(sid))
        .map(|s| Arc::from(s.as_ref()))
}

fn resolve_string(graph: &CodeGraph, id: sqry_core::graph::unified::StringId) -> Arc<str> {
    graph
        .strings()
        .resolve(id)
        .map_or_else(|| Arc::from("<unresolved>"), |s| Arc::from(s.as_ref()))
}

/// Lower an [`EdgeKind`] into its canonical form.
fn canonicalize_edge_kind(graph: &CodeGraph, kind: &EdgeKind) -> CanonicalEdgeKind {
    match kind {
        EdgeKind::Defines => CanonicalEdgeKind::Defines,
        EdgeKind::Contains => CanonicalEdgeKind::Contains,
        EdgeKind::Calls {
            argument_count,
            is_async,
            ..
        } => CanonicalEdgeKind::Calls {
            argument_count: *argument_count,
            is_async: *is_async,
        },
        EdgeKind::References => CanonicalEdgeKind::References,
        EdgeKind::Imports { alias, is_wildcard } => CanonicalEdgeKind::Imports {
            alias: resolve_opt_string(graph, *alias),
            is_wildcard: *is_wildcard,
        },
        EdgeKind::Exports { kind, alias } => CanonicalEdgeKind::Exports {
            kind: *kind,
            alias: resolve_opt_string(graph, *alias),
        },
        EdgeKind::TypeOf {
            context,
            index,
            name,
        } => CanonicalEdgeKind::TypeOf {
            context: *context,
            index: *index,
            name: resolve_opt_string(graph, *name),
        },
        EdgeKind::Inherits => CanonicalEdgeKind::Inherits,
        EdgeKind::Implements => CanonicalEdgeKind::Implements,
        EdgeKind::LifetimeConstraint { constraint_kind } => CanonicalEdgeKind::LifetimeConstraint {
            constraint_kind: *constraint_kind,
        },
        EdgeKind::TraitMethodBinding {
            trait_name,
            impl_type,
            is_ambiguous,
        } => CanonicalEdgeKind::TraitMethodBinding {
            trait_name: resolve_string(graph, *trait_name),
            impl_type: resolve_string(graph, *impl_type),
            is_ambiguous: *is_ambiguous,
        },
        EdgeKind::MacroExpansion {
            expansion_kind,
            is_verified,
        } => CanonicalEdgeKind::MacroExpansion {
            expansion_kind: *expansion_kind,
            is_verified: *is_verified,
        },
        EdgeKind::FfiCall { convention } => CanonicalEdgeKind::FfiCall {
            convention: *convention,
        },
        EdgeKind::HttpRequest { method, url } => CanonicalEdgeKind::HttpRequest {
            method: *method,
            url: resolve_opt_string(graph, *url),
        },
        EdgeKind::GrpcCall { service, method } => CanonicalEdgeKind::GrpcCall {
            service: resolve_string(graph, *service),
            method: resolve_string(graph, *method),
        },
        EdgeKind::WebAssemblyCall => CanonicalEdgeKind::WebAssemblyCall,
        EdgeKind::DbQuery { query_type, table } => CanonicalEdgeKind::DbQuery {
            query_type: *query_type,
            table: resolve_opt_string(graph, *table),
        },
        EdgeKind::TableRead { table_name, schema } => CanonicalEdgeKind::TableRead {
            table_name: resolve_string(graph, *table_name),
            schema: resolve_opt_string(graph, *schema),
        },
        EdgeKind::TableWrite {
            table_name,
            schema,
            operation,
        } => CanonicalEdgeKind::TableWrite {
            table_name: resolve_string(graph, *table_name),
            schema: resolve_opt_string(graph, *schema),
            operation: *operation,
        },
        EdgeKind::TriggeredBy {
            trigger_name,
            schema,
        } => CanonicalEdgeKind::TriggeredBy {
            trigger_name: resolve_string(graph, *trigger_name),
            schema: resolve_opt_string(graph, *schema),
        },
        EdgeKind::MessageQueue { protocol, topic } => CanonicalEdgeKind::MessageQueue {
            protocol: canonicalize_mq_protocol(graph, protocol),
            topic: resolve_opt_string(graph, *topic),
        },
        EdgeKind::WebSocket { event } => CanonicalEdgeKind::WebSocket {
            event: resolve_opt_string(graph, *event),
        },
        EdgeKind::GraphQLOperation { operation } => CanonicalEdgeKind::GraphQLOperation {
            operation: resolve_string(graph, *operation),
        },
        EdgeKind::ProcessExec { command } => CanonicalEdgeKind::ProcessExec {
            command: resolve_string(graph, *command),
        },
        EdgeKind::FileIpc { path_pattern } => CanonicalEdgeKind::FileIpc {
            path_pattern: resolve_opt_string(graph, *path_pattern),
        },
        EdgeKind::ProtocolCall { protocol, metadata } => CanonicalEdgeKind::ProtocolCall {
            protocol: resolve_string(graph, *protocol),
            metadata: resolve_opt_string(graph, *metadata),
        },
        EdgeKind::GenericBound => CanonicalEdgeKind::GenericBound,
        EdgeKind::AnnotatedWith => CanonicalEdgeKind::AnnotatedWith,
        EdgeKind::AnnotationParam => CanonicalEdgeKind::AnnotationParam,
        EdgeKind::LambdaCaptures => CanonicalEdgeKind::LambdaCaptures,
        EdgeKind::ModuleExports => CanonicalEdgeKind::ModuleExports,
        EdgeKind::ModuleRequires => CanonicalEdgeKind::ModuleRequires,
        EdgeKind::ModuleOpens => CanonicalEdgeKind::ModuleOpens,
        EdgeKind::ModuleProvides => CanonicalEdgeKind::ModuleProvides,
        EdgeKind::TypeArgument => CanonicalEdgeKind::TypeArgument,
        EdgeKind::ExtensionReceiver => CanonicalEdgeKind::ExtensionReceiver,
        EdgeKind::CompanionOf => CanonicalEdgeKind::CompanionOf,
        EdgeKind::SealedPermit => CanonicalEdgeKind::SealedPermit,
        EdgeKind::Wraps {
            kind,
            chain_position,
        } => CanonicalEdgeKind::Wraps {
            kind: *kind,
            chain_position: *chain_position,
        },
        EdgeKind::ChannelPeer {
            direction,
            buffer_kind,
        } => CanonicalEdgeKind::ChannelPeer {
            direction: *direction,
            buffer_kind: *buffer_kind,
        },
        EdgeKind::Instantiates {
            type_args,
            inference_kind,
        } => CanonicalEdgeKind::Instantiates {
            type_args: type_args
                .iter()
                .map(|ta| (resolve_string(graph, ta.name), ta.default_typed))
                .collect(),
            inference_kind: *inference_kind,
        },
    }
}

fn canonicalize_mq_protocol(graph: &CodeGraph, protocol: &MqProtocol) -> CanonicalMqProtocol {
    match protocol {
        MqProtocol::Kafka => CanonicalMqProtocol::Kafka,
        MqProtocol::Sqs => CanonicalMqProtocol::Sqs,
        MqProtocol::RabbitMq => CanonicalMqProtocol::RabbitMq,
        MqProtocol::Nats => CanonicalMqProtocol::Nats,
        MqProtocol::Redis => CanonicalMqProtocol::Redis,
        MqProtocol::Other(sid) => CanonicalMqProtocol::Other(resolve_string(graph, *sid)),
    }
}

/// Assert two [`SemGraph`]s are semantically equivalent. Panics with a
/// detailed diff on mismatch. `context` is included in the panic message
/// so the proptest shrinker can report which operator sequence triggered
/// the divergence.
///
/// The comparison is set-based: node ordering, edge ordering, and any
/// allocation-order artifacts are intentionally invisible.
pub fn assert_graph_semantically_equivalent(
    baseline: &SemGraph,
    candidate: &SemGraph,
    context: &str,
) {
    let baseline_nodes: BTreeSet<&NodeSemKey> = baseline.keys().collect();
    let candidate_nodes: BTreeSet<&NodeSemKey> = candidate.keys().collect();

    let only_in_baseline: Vec<&NodeSemKey> = baseline_nodes
        .difference(&candidate_nodes)
        .copied()
        .collect();
    let only_in_candidate: Vec<&NodeSemKey> = candidate_nodes
        .difference(&baseline_nodes)
        .copied()
        .collect();

    if !only_in_baseline.is_empty() || !only_in_candidate.is_empty() {
        panic!(
            "[{context}] node sets differ:\n  only in baseline ({}): {:#?}\n  only in candidate ({}): {:#?}",
            only_in_baseline.len(),
            &only_in_baseline[..only_in_baseline.len().min(10)],
            only_in_candidate.len(),
            &only_in_candidate[..only_in_candidate.len().min(10)],
        );
    }

    // Node sets match — compare edge sets per node. Each edge set is a
    // `HashSet<EdgeSemKey>`; set difference works identically to
    // BTreeSet::difference at the semantic level.
    for node_key in &baseline_nodes {
        let empty = HashSet::new();
        let baseline_edges: &HashSet<EdgeSemKey> = baseline.get(*node_key).unwrap_or(&empty);
        let candidate_edges: &HashSet<EdgeSemKey> = candidate.get(*node_key).unwrap_or(&empty);

        let only_in_baseline: Vec<&EdgeSemKey> =
            baseline_edges.difference(candidate_edges).collect();
        let only_in_candidate: Vec<&EdgeSemKey> =
            candidate_edges.difference(baseline_edges).collect();

        if !only_in_baseline.is_empty() || !only_in_candidate.is_empty() {
            panic!(
                "[{context}] edge sets differ at node {:?}:\n  only in baseline ({}): {:#?}\n  only in candidate ({}): {:#?}",
                node_key,
                only_in_baseline.len(),
                &only_in_baseline[..only_in_baseline.len().min(10)],
                only_in_candidate.len(),
                &only_in_candidate[..only_in_candidate.len().min(10)],
            );
        }
    }
}

/// Assert that two build-result pairs report the same error-or-success
/// status. The harness always produces full filesystem states before
/// building, so Ok-vs-Err divergence would indicate a real regression in
/// how the incremental engine surfaces errors — exactly what we want to
/// catch.
///
/// Error messages themselves are *not* compared (they include tempdir
/// paths and line numbers that legitimately differ). Only the Ok/Err
/// discriminant and, when both are Err, that both errors exist at all.
pub fn assert_build_errors_equivalent<T, E: std::fmt::Debug>(
    baseline: &Result<T, E>,
    candidate: &Result<T, E>,
    context: &str,
) {
    match (baseline, candidate) {
        (Ok(_), Ok(_)) => {}
        (Err(_), Err(_)) => {}
        (Ok(_), Err(err)) => {
            panic!("[{context}] baseline build succeeded but candidate failed: {err:?}",)
        }
        (Err(err), Ok(_)) => panic!(
            "[{context}] baseline build failed ({err:?}) but candidate succeeded — engine \
             smothered a real error"
        ),
    }
}

/// Convenience: build a [`SemGraph`] from a [`Result<CodeGraph, E>`]
/// for the Ok case (and return None for the Err case).
pub fn sem_graph_from_result<E>(
    result: &Result<CodeGraph, E>,
    workspace_root: &Path,
) -> Option<SemGraph> {
    result
        .as_ref()
        .ok()
        .map(|graph| build_sem_graph(graph, workspace_root))
}

/// Resolve a workspace-root path to its canonical absolute form, matching
/// the convention used inside [`CodeGraph::files()`]. Using the same
/// canonical form in both the harness and the build ensures
/// [`canonical_file_path`] strips prefixes correctly.
///
/// Falls back to the input path unchanged when canonicalization fails
/// (e.g., missing directory) — callers typically pass a path from a
/// tempdir that definitely exists, so the fallback is defensive.
#[must_use]
pub fn canonicalize_workspace_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}
