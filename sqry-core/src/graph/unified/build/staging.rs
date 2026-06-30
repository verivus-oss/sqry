//! Staging graph for transactional builds.
//!
//! This module implements the staging buffer that provides transactional
//! semantics for graph builds.
//!
//! # Overview
//!
//! The [`StagingGraph`] collects all changes (node additions, edge additions)
//! in a staging buffer. Changes are only applied to the main graph on commit.
//! On failure, changes can be rolled back (discarded) leaving the graph unchanged.
//!
//! # Usage
//!
//! ```ignore
//! let mut staging = StagingGraph::new();
//!
//! // Stage operations
//! staging.add_node(node_entry);
//! staging.add_edge(source, target, kind, file);
//!
//! // On success, commit to main graph
//! staging.commit(&mut graph)?;
//!
//! // On failure, rollback discards all changes
//! staging.rollback();
//! ```
//!
//! # Thread Safety
//!
//! The `StagingGraph` itself is not thread-safe - it's intended to be used
//! by a single builder thread. However, the commit operation acquires
//! appropriate locks on the main graph for safe mutation.

use std::collections::HashMap;

use std::collections::HashSet;

#[cfg(test)]
use super::super::edge::ResolvedVia;
use super::super::edge::{EdgeKind, MqProtocol};
use super::super::file::FileId;
use super::super::node::NodeId;
use super::super::resolution::display_graph_qualified_name;
use super::super::storage::c_indirect::{BindingSiteKind, IndirectShape, LocalScopeIndex};
use super::super::storage::{NodeArena, NodeEntry, StringInterner};
use super::super::string::StringId;
use super::pass3_intra::PendingEdge;
use crate::confidence::ConfidenceMetadata;
use crate::graph::node::Language;

/// Operation staged for later commit.
#[derive(Debug, Clone)]
pub enum StagingOp {
    /// Add a node to the arena.
    AddNode {
        /// Node entry to add.
        entry: NodeEntry,
        /// Expected `NodeId` after allocation.
        expected_id: Option<NodeId>,
    },
    /// Add an edge to the graph.
    AddEdge {
        /// Source node.
        source: NodeId,
        /// Target node.
        target: NodeId,
        /// Edge kind.
        kind: EdgeKind,
        /// File containing the edge.
        file: FileId,
        /// Source spans of the edge (e.g., call site locations for LSP call hierarchy).
        spans: Vec<crate::graph::node::Span>,
    },
    /// Register a file in the file registry.
    RegisterFile {
        /// File path (as string for serialization).
        path: String,
        /// Expected `FileId`.
        expected_id: FileId,
    },
    /// Intern a string with a local staging ID.
    ///
    /// The `local_id` is the staging-local `StringId` allocated by the helper.
    /// During `commit_strings()`, this local ID will be remapped to a global ID
    /// in the `StringInterner`.
    InternString {
        /// Local staging `StringId` (allocated sequentially per-file).
        local_id: crate::graph::unified::StringId,
        /// String to intern.
        value: String,
    },
}

/// Statistics from staging operations.
#[derive(Debug, Clone, Default)]
pub struct StagingStats {
    /// Number of nodes staged.
    pub nodes_staged: usize,
    /// Number of edges staged.
    pub edges_staged: usize,
    /// Number of files registered.
    pub files_registered: usize,
    /// Number of strings interned.
    pub strings_interned: usize,
}

// ---------------------------------------------------------------------------
// C indirect-call precision: per-file staging payload (Phase A, U10).
// ---------------------------------------------------------------------------

/// Per-file staging payload populated by the C plugin's Phase 1 walkers.
///
/// During Phase 1 (parallel parse), the C plugin classifies address-taken
/// sites, indirect callsites, struct function-pointer field signatures, and
/// binding-plane entries into this payload. The per-file payload is then
/// drained during Phase 3 commit (U11) into the workspace-global
/// [`CIndirectSideTables`](super::super::storage::c_indirect::CIndirectSideTables)
/// on [`crate::graph::unified::CodeGraph`].
///
/// All fields are keyed by **qualified-name strings** rather than `NodeId`s
/// because the final `NodeIds` aren't known until after Phase 3 commit +
/// Phase 4c-prime cross-file unification. U11's drain resolves names →
/// `NodeIds` via the unified workspace qualified-name index.
///
/// `pending_address_taken_names` carries staging-local [`StringId`]s
/// (DESIGN §2.5) — the helper interns each name through the standard
/// per-file staging interner, and U11 resolves the local IDs via the
/// remap table produced by [`Self::commit_strings`] before looking them
/// up in the qualified-name index. The remaining payload fields
/// (`pending_struct_field_signatures`, `pending_bindings`,
/// `pending_indirect_callsites`) keep their composite qualified-name
/// strings verbatim because U11 needs the post-unification canonical
/// names directly, with no remap step in between.
///
/// Non-C plugins never construct this payload — the parent
/// `Option<Box<CIndirectStagingPayload>>` slot on [`StagingGraph`] stays
/// `None`, keeping the per-file staging buffer's size unchanged for the
/// other 36 language plugins.
///
/// Wire contract: this type is **not** serialised. It is a Phase 1 → Phase
/// 3 hand-off only. The persisted shape lives on
/// [`CIndirectSideTables`](super::super::storage::c_indirect::CIndirectSideTables).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CIndirectStagingPayload {
    /// Staging-local [`StringId`]s of every function whose address is
    /// taken anywhere in this file (DESIGN §2.5 pattern table).
    ///
    /// Each entry is the local `StringId` returned by the helper's
    /// standard string interner — interned via the same path as every
    /// other staging string so U11 can remap local → global IDs through
    /// the table produced by [`Self::commit_strings`], then resolve each
    /// global ID to a canonical `NodeId` via the post-unification
    /// qualified-name index. The flag is applied via
    /// [`crate::graph::unified::storage::metadata::NodeFlags::ADDRESS_TAKEN`]
    /// using `mark_address_taken`.
    ///
    /// May contain duplicates within a file (a function can be
    /// address-taken at multiple sites); U11 deduplicates implicitly via
    /// idempotent `mark_address_taken` calls. Tests can resolve a
    /// `StringId` back to its source string via
    /// [`Self::resolve_local_string`].
    pub pending_address_taken_names: Vec<StringId>,

    /// Function-pointer field signatures discovered in struct declarations.
    ///
    /// Each tuple is `(struct_tag, field_name, signature)`, where
    /// `signature` is the canonical signature string produced by
    /// `signature_builder::build_function_signature` (DESIGN §3.1 grammar).
    /// U11's drain interns each leg and inserts into
    /// `CIndirectSideTables::struct_field_fnptr`.
    pub pending_struct_field_signatures: Vec<(String, String, String)>,

    /// Tentative binding entries (designated + positional initializers).
    ///
    /// Each entry records a `(struct_tag, field_name) → target_function`
    /// binding plus the enclosing instance variable name. U11 resolves
    /// `instance_name` + `target_fn_name` to canonical `NodeIds`, then
    /// inserts into `CIndirectSideTables::bindings_by_field`.
    pub pending_bindings: Vec<PendingBinding>,

    /// Indirect callsites captured during Phase 1.
    ///
    /// Each callsite carries the caller's qualified name (resolved to a
    /// `NodeId` in U11), use-span (for `LocalScopeIndex::resolve_type`
    /// lookups in U12's resolver), syntactic shape, argument count, and
    /// `is_async` flag. U11 drains these into
    /// `CIndirectSideTables::pending_callsites`.
    pub pending_indirect_callsites: Vec<PendingIndirectCallsite>,

    /// Per-file block-scope arena (DESIGN §4.1).
    ///
    /// The C plugin builds this via `build_local_scope_index`; U11
    /// transfers it (keyed by `FileId`) into
    /// `CIndirectSideTables::local_scope_indices`.
    pub local_scope_index: Option<LocalScopeIndex>,
}

/// One binding-plane entry staged during Phase 1 (DESIGN §2.5 / §7.1).
///
/// Names are kept as strings rather than `NodeIds` because the final `NodeIds`
/// aren't known until after Phase 3 + Phase 4c-prime unification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBinding {
    /// Canonical struct tag (e.g. `file_operations`). Matches the qualified
    /// name used by `helper.add_struct(...)` for the enclosing struct
    /// declaration.
    pub struct_tag: String,
    /// Field name within the struct (e.g. `read`).
    pub field_name: String,
    /// Qualified name of the enclosing instance variable (e.g.
    /// `ext4_file_operations`). Resolved to a `NodeId` in U11.
    pub instance_name: String,
    /// Qualified name of the address-taken target function. Resolved to a
    /// `NodeId` in U11.
    pub target_fn_name: String,
    /// Designated vs positional initializer (DESIGN §2.5).
    pub site_kind: BindingSiteKind,
}

/// One indirect callsite staged during Phase 1 (DESIGN §4.2).
///
/// The final `IndirectCallsite` stored on
/// [`CIndirectSideTables`](super::super::storage::c_indirect::CIndirectSideTables)
/// uses `caller: NodeId` + `file_id: FileId`; this staging form keeps the
/// caller's qualified name string (resolved to `NodeId` in U11) and lets U11
/// stamp the `file_id` from the per-file context during drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingIndirectCallsite {
    /// Qualified name of the enclosing function/method that issued the call.
    /// Resolved to a `NodeId` in U11 via the post-unification qualified-name
    /// index.
    pub caller_qualified_name: String,
    /// Byte range of the callsite expression in the source file.
    pub use_span: (usize, usize),
    /// Syntactic shape (pointer-expression vs field-expression call).
    pub shape: IndirectShape,
    /// Argument count carried from the Phase 1 `Calls` edge — re-stamped
    /// on each rewritten precise edge by U12.
    pub argument_count: u32,
    /// `is_async` carried from the Phase 1 `Calls` edge.
    pub is_async: bool,
}

/// Metadata flags that are monotonic when merging duplicate staged nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeMetadataFlag {
    /// The staged node represents async code.
    Async,
    /// The staged node represents static code.
    Static,
    /// The staged node represents unsafe code.
    Unsafe,
    /// The staged node is a real source declaration.
    Definition,
}

/// Compact set of [`NodeMetadataFlag`] values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeMetadataFlags {
    bits: u8,
}

impl NodeMetadataFlags {
    const ASYNC: u8 = 1 << 0;
    const STATIC: u8 = 1 << 1;
    const UNSAFE: u8 = 1 << 2;
    const DEFINITION: u8 = 1 << 3;

    /// Returns an empty flag set.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// Adds `flag` to the set.
    pub fn insert(&mut self, flag: NodeMetadataFlag) {
        self.bits |= Self::bit(flag);
    }

    /// Returns true when `flag` is present.
    #[must_use]
    pub const fn contains(self, flag: NodeMetadataFlag) -> bool {
        self.bits & Self::bit(flag) != 0
    }

    const fn bit(flag: NodeMetadataFlag) -> u8 {
        match flag {
            NodeMetadataFlag::Async => Self::ASYNC,
            NodeMetadataFlag::Static => Self::STATIC,
            NodeMetadataFlag::Unsafe => Self::UNSAFE,
            NodeMetadataFlag::Definition => Self::DEFINITION,
        }
    }
}

/// Metadata to merge into an existing staged node.
#[derive(Debug, Clone, Default)]
pub struct NodeMetadataUpdate {
    span: Option<crate::graph::node::Span>,
    flags: NodeMetadataFlags,
    visibility: Option<StringId>,
    signature: Option<StringId>,
}

impl NodeMetadataUpdate {
    /// Returns an update with no fields set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a span update when `span` is present.
    #[must_use]
    pub fn with_optional_span(mut self, span: Option<crate::graph::node::Span>) -> Self {
        self.span = span;
        self
    }

    /// Marks `flag` when `enabled` is true.
    #[must_use]
    pub fn mark_if(mut self, flag: NodeMetadataFlag, enabled: bool) -> Self {
        if enabled {
            self.flags.insert(flag);
        }
        self
    }

    /// Adds a visibility update when `visibility` is present.
    #[must_use]
    pub fn with_optional_visibility(mut self, visibility: Option<StringId>) -> Self {
        self.visibility = visibility;
        self
    }

    /// Adds a signature update when `signature` is present.
    #[must_use]
    pub fn with_optional_signature(mut self, signature: Option<StringId>) -> Self {
        self.signature = signature;
        self
    }
}

/// Error during staging commit.
#[derive(Debug, Clone)]
pub enum StagingError {
    /// Node allocation failed.
    NodeAllocationFailed {
        /// Error description.
        reason: String,
    },
    /// Edge addition failed.
    EdgeAdditionFailed {
        /// Error description.
        reason: String,
    },
    /// File registration failed.
    FileRegistrationFailed {
        /// Error description.
        reason: String,
    },
    /// Commit was aborted.
    Aborted {
        /// Reason for abort.
        reason: String,
    },
    /// Duplicate local `StringId` detected during staging.
    ///
    /// This indicates misuse of the staging API - each local `StringId`
    /// should be unique within a single `StagingGraph` instance.
    DuplicateLocalStringId {
        /// The duplicate local `StringId`.
        local_id: crate::graph::unified::StringId,
    },
    /// String interner capacity exhausted.
    ///
    /// The `StringInterner` has reached its maximum capacity and cannot
    /// allocate any more string IDs.
    InternCapacityExhausted,
    /// An `InternString` operation contained a non-local `StringId`.
    ///
    /// `StagingOp::InternString.local_id` MUST be a staging-local `StringId` created
    /// with `StringId::new_local(...)`.
    NonLocalInternStringId {
        /// The non-local (global) `StringId`.
        id: crate::graph::unified::StringId,
    },
    /// A staging-local `StringId` was encountered during remap but was not present
    /// in the remap table.
    UnmappedLocalStringId {
        /// The missing staging-local `StringId`.
        local_id: crate::graph::unified::StringId,
        /// Human-readable location of the carrier field.
        carrier: &'static str,
    },
    /// A required `StringId` carrier contained `StringId::INVALID`.
    InvalidRequiredStringId {
        /// The invalid `StringId`.
        id: crate::graph::unified::StringId,
        /// Human-readable location of the carrier field.
        carrier: &'static str,
    },
}

impl std::fmt::Display for StagingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeAllocationFailed { reason } => {
                write!(f, "Node allocation failed: {reason}")
            }
            Self::EdgeAdditionFailed { reason } => {
                write!(f, "Edge addition failed: {reason}")
            }
            Self::FileRegistrationFailed { reason } => {
                write!(f, "File registration failed: {reason}")
            }
            Self::Aborted { reason } => {
                write!(f, "Commit aborted: {reason}")
            }
            Self::DuplicateLocalStringId { local_id } => {
                write!(
                    f,
                    "Duplicate local StringId detected: index={}",
                    local_id.index()
                )
            }
            Self::InternCapacityExhausted => {
                write!(f, "String interner capacity exhausted")
            }
            Self::NonLocalInternStringId { id } => {
                write!(
                    f,
                    "InternString.local_id must be staging-local, got {id:?} (index={})",
                    id.index()
                )
            }
            Self::UnmappedLocalStringId { local_id, carrier } => {
                write!(
                    f,
                    "Unmapped staging-local StringId {local_id:?} (index={}) in {carrier}",
                    local_id.index()
                )
            }
            Self::InvalidRequiredStringId { id, carrier } => {
                write!(f, "Invalid required StringId {id:?} in {carrier}")
            }
        }
    }
}

impl std::error::Error for StagingError {}

/// Apply metadata updates to a staged `NodeEntry`.
///
/// Updates span, async/static/unsafe flags, visibility, and signature on the entry.
fn apply_node_metadata(entry: &mut NodeEntry, update: &NodeMetadataUpdate) {
    if let Some(span) = &update.span {
        apply_span_to_entry(entry, span);
    }

    if update.flags.contains(NodeMetadataFlag::Async) {
        entry.is_async = true;
    }

    if update.flags.contains(NodeMetadataFlag::Static) {
        entry.is_static = true;
    }

    if update.flags.contains(NodeMetadataFlag::Unsafe) {
        entry.is_unsafe = true;
    }

    // Monotonic OR-in: once a node is known to be a real declaration, never
    // clear the signal (stub-first/declaration-later and the reverse both
    // converge to `true`).
    if update.flags.contains(NodeMetadataFlag::Definition) {
        entry.is_definition = true;
    }

    if entry.visibility.is_none()
        && let Some(vis) = update.visibility
    {
        entry.visibility = Some(vis);
    }

    if entry.signature.is_none()
        && let Some(sig) = update.signature
    {
        entry.signature = Some(sig);
    }
}

/// Apply a source span to a `NodeEntry`, updating line/column info if the new span
/// extends the existing range.
fn apply_span_to_entry(entry: &mut NodeEntry, span: &crate::graph::node::Span) {
    let start_line = u32::try_from(span.start.line.saturating_add(1)).unwrap_or(u32::MAX);
    let start_column = u32::try_from(span.start.column).unwrap_or(u32::MAX);
    let end_line = u32::try_from(span.end.line.saturating_add(1)).unwrap_or(u32::MAX);
    let end_column = u32::try_from(span.end.column).unwrap_or(u32::MAX);

    let should_update = entry.start_line == 0
        || entry.end_line == 0
        || end_line > entry.end_line
        || (end_line == entry.end_line && end_column > entry.end_column);

    if should_update {
        entry.start_line = start_line;
        entry.start_column = start_column;
        entry.end_line = end_line;
        entry.end_column = end_column;
    }
}

// =======================================================================
// Go-plugin side-channel hints (Go T1 implements-and-promotion, Cluster A)
// =======================================================================
//
// `GoHints` is a build-time scratch carrier populated by the Go plugin's
// Phase-1 parser and consumed post-Phase-4e by
// `pass_go_method_set_satisfaction`. It is **not** part of the graph's
// persisted state (V10 snapshot is unchanged) — the live build target
// holds an instance only for the duration of one build, after which the
// pass drains it.
//
// Each hint carries the staging-local `NodeId`s / `StringId`s that the
// plugin observed. Phase 3's commit path is responsible for remapping
// those identities to their post-commit global values before flushing
// the hint set into the live target via
// [`GraphMutationTarget::go_hints_mut`][crate::graph::unified::mutation_target::GraphMutationTarget::go_hints_mut].

/// Aggregate of all Go-plugin Phase-1 observations the method-set
/// satisfaction pass needs at run time.
///
/// Plugins push into `embeddings`, `named_type_conversions`,
/// `receiver_calls`, and `method_receivers` during Phase 1 parse; the
/// pass consumes them after Phase 4e completes.
#[derive(Debug, Default, Clone)]
pub struct GoHints {
    /// Struct embedding observations:
    /// `outer.<embedded field> : inner_type` per Go spec §"Struct types".
    pub embeddings: Vec<GoEmbeddingHint>,
    /// Explicit named-type conversion call sites: `T(expr)` where `T`
    /// resolves to a named type.
    pub named_type_conversions: Vec<GoNamedTypeConversionHint>,
    /// Receiver-method call sites: `recv.M(args)` where `recv` is
    /// classified by [`GoReceiverHintKind`].
    pub receiver_calls: Vec<GoReceiverCallHint>,
    /// Receiver-pointerness side channel for each Go method declaration.
    /// Recovers the `*T` vs `T` distinction that the Go plugin's
    /// `strip_receiver_modifiers` collapses out of the canonical
    /// qualified-name shape. Consumed by Cluster D2 to drive Go-spec
    /// §"Method sets" value- vs pointer-bucket method-set composition.
    pub method_receivers: Vec<GoMethodReceiverHint>,
    /// Canonical signature side channel for each Go method declaration.
    /// Recovers the parameter+result type sequence at emission time so
    /// the post-Phase-4e satisfaction pass can compare interface
    /// method signatures against candidate method signatures byte-for-
    /// byte. Without this hint the param/return texts would be lost
    /// behind the Method node's qualified-name shape, exactly as
    /// pointerness was lost before [`GoMethodReceiverHint`] existed.
    /// Consumed by Cluster D3's tightened T1.1 satisfaction predicate.
    pub method_signatures: Vec<GoMethodSignatureHint>,
    /// Canonical signature side channel for each Go function
    /// declaration and each named function-type declaration. Drives
    /// Cluster D3's T1.3 function-signature implementations
    /// (`Implements(fn → F)` where `F` is a named function type whose
    /// underlying signature equals `fn`'s signature). The Type-node form
    /// (named function-type underlying signature) and the
    /// Function-node form (bare function declaration signature) share
    /// the same hint struct because both carry the same `(NodeId,
    /// canonical_signature, FileId)` triple — the consumer
    /// disambiguates by inspecting the node's kind in the live graph.
    pub function_signatures: Vec<GoFunctionSignatureHint>,
}

/// One observed struct embedding.
///
/// The pass uses this to build the embedding adjacency graph
/// (`02_DESIGN` §4.2 step 1). `pointerness` records whether the embedded
/// field is `T` (value) or `*T` (pointer) — the Go method-set rules
/// differ on this.
#[derive(Debug, Clone, Copy)]
pub struct GoEmbeddingHint {
    /// `NodeId` of the outer struct (the one containing the embedded
    /// field).
    pub outer: NodeId,
    /// Interned qualified name of the embedded type, looked up against
    /// the by-qualified-name index at pass run time.
    pub inner_qualified_name: StringId,
    /// Whether the embed is `T` (value) or `*T` (pointer).
    pub pointerness: crate::graph::unified::mutation_target::Receiver,
    /// File in which the embedding was observed.
    pub file: FileId,
}

/// One observed `T(expr)` named-type conversion call site.
///
/// The pass uses this to drive T1.3 function-signature implementations:
/// when `T` is a named function type, a conversion `T(f)` for a function
/// `f` whose signature matches `T`'s signature contributes an
/// `Implements(f → T)` edge.
#[derive(Debug, Clone, Copy)]
pub struct GoNamedTypeConversionHint {
    /// `NodeId` of the call expression (`CallSite` or equivalent).
    pub call_site: NodeId,
    /// Interned qualified name of the target named type `T`.
    pub target_type_qualified_name: StringId,
    /// `NodeId` of the argument expression `expr` inside `T(expr)`.
    pub argument_node: NodeId,
    /// File in which the conversion was observed.
    pub file: FileId,
}

/// One observed `recv.M(args)` receiver-method call.
///
/// The pass uses this to shadow `Calls` / `References` edges over
/// promoted methods (`02_DESIGN` §4.2 step 5–6). The receiver expression
/// is classified by [`GoReceiverHintKind`] into one of four resolution
/// shapes.
#[derive(Debug, Clone)]
pub struct GoReceiverCallHint {
    /// `NodeId` of the call expression.
    pub call_site: NodeId,
    /// `NodeId` of the method node the plugin already resolved this
    /// call to, on the original receiver type. The pass walks
    /// promotion-chain to mint shadow calls into promoted-method
    /// shadows.
    pub callee_method: NodeId,
    /// Interned method name (e.g. `"M"`).
    pub method_name: StringId,
    /// Receiver classification — determines which resolution path the
    /// pass takes.
    pub receiver: GoReceiverHintKind,
    /// Argument count at the call site, mirrors `EdgeKind::Calls
    /// { argument_count }`.
    pub argument_count: u8,
    /// Whether the call expression is awaited (Go: `go func()`-like
    /// async usage; preserved for parity with `EdgeKind::Calls`).
    pub is_async: bool,
    /// File in which the call was observed.
    pub file: FileId,
}

/// Receiver-pointerness side channel for one Go method declaration.
///
/// Recovers the `*T` vs `T` distinction that the Go plugin's
/// `strip_receiver_modifiers` (sqry-lang-go/src/relations/graph_builder.rs:1237)
/// collapses out of the canonical method qualified name. The plugin emits
/// methods declared with receiver `*T` and methods declared with receiver
/// `T` under a single canonical qualified-name shape
/// `<pkg>.<T>.<MethodName>`, which is sufficient for call-resolution but
/// loses the pointerness signal that the Go method-set rules
/// (Go spec §"Method sets") rely on:
///
/// - `MethodSet(T)` is the set of methods declared with receiver `T`.
/// - `MethodSet(*T)` is `MethodSet(T)` ∪ methods declared with receiver `*T`.
///
/// Cluster D2's T1.1 implicit-interface-satisfaction pass needs this
/// distinction to compute correct value-bucket vs pointer-bucket method
/// sets. Without it, D1 conservatively routed every promoted method
/// through the value bucket; D2's classifier uses this hint to lift
/// the pointer-only methods out of the value bucket.
///
/// The hint is also consumed by D2's tightened shadow-`Calls` /
/// `References` emission, where it lets the pass distinguish methods
/// that can be promoted to `S` from methods that can only be promoted
/// to `*S`.
#[derive(Debug, Clone, Copy)]
pub struct GoMethodReceiverHint {
    /// `NodeId` of the `Method` node minted by the Go plugin's Phase-1
    /// pass for this method declaration.
    pub method_node: NodeId,
    /// Interned qualified name of the receiver type, *without* the
    /// pointer-prefix and type-argument suffix (i.e. the canonical
    /// `<pkg>.<T>` form, matching the receiver-side `strip_receiver_modifiers`
    /// output). Looked up against the by-qualified-name index at pass
    /// run time to recover the canonical receiver-`Struct` / `Type`
    /// `NodeId`.
    pub receiver_type_qualified_name: StringId,
    /// Whether the method was declared with receiver `*T` (Pointer) or
    /// `T` (Value). Derived syntactically from the receiver-text's
    /// leading `*` at hint-emission time.
    pub receiver_pointerness: crate::graph::unified::mutation_target::Receiver,
    /// File in which the method was declared.
    pub file: FileId,
}

/// Canonical-signature side channel for one Go method declaration.
///
/// Recovers the parameter+result lexical sequence collapsed by Cluster
/// B/C's per-method emission. The plugin records the canonical-signature
/// bytes computed by
/// [`canonicalise_go_signature`][crate::graph::unified::build::go_signature::canonicalise_go_signature]
/// at the receiver-method declaration site (for top-level methods) and
/// at each `method_elem` inside an interface declaration (for interface
/// methods). The signature is the same lexical shape the post-Phase-4e
/// `pass_go_method_set_satisfaction` consumes when comparing a candidate
/// method against an interface method — keying on canonical bytes
/// guarantees the comparison is exactly the predicate prescribed by
/// `docs/development/go-implements-and-promotion/02_DESIGN.md` §4.1.3.
///
/// Cluster D3's T1.1 tightened predicate consumes this hint: a method
/// `m_c` on candidate `C` satisfies interface method `m_I` iff the two
/// share the same name **and** the same canonical signature. The
/// previously-shipped Cluster D2 satisfaction predicate (name-only)
/// remains as a fallback for nodes that lack a signature hint (e.g. unit
/// test fixtures that mint Method nodes directly without invoking the
/// plugin); production builds emit a hint for every method node so the
/// production path always uses the tightened predicate.
#[derive(Debug, Clone)]
pub struct GoMethodSignatureHint {
    /// `NodeId` of the `Method` node the signature describes. For
    /// top-level method declarations this matches
    /// [`GoMethodReceiverHint::method_node`]; for interface method
    /// elements this is the Method node minted inside
    /// `process_interface_method_elem`.
    pub method_node: NodeId,
    /// Canonical signature bytes produced by
    /// [`canonicalise_go_signature`][crate::graph::unified::build::go_signature::canonicalise_go_signature].
    /// Stored as a `String` (not `StringId`) because the bytes are
    /// derived from per-method lexical context and would inflate the
    /// global interner without measurable lookup-time benefit — the
    /// satisfaction pass builds an ephemeral `BTreeMap<NodeId, String>`
    /// and discards it post-pass.
    pub canonical_signature: String,
    /// File in which the method was declared.
    pub file: FileId,
}

/// Canonical-signature side channel for one Go function declaration or
/// one named function-type underlying signature.
///
/// Drives Cluster D3's T1.3 implementation. Two emission shapes share
/// this hint:
///
/// 1. **Function declarations** (`func foo(x int) error { ... }`):
///    `function_node` is the `NodeKind::Function` `NodeId`; the canonical
///    signature is the function's own param+result. The bare function
///    becomes a T1.3 candidate when its signature matches a named
///    function-type's underlying signature.
/// 2. **Named function types** (`type HandlerFunc func(w
///    http.ResponseWriter, r *http.Request)`): `function_node` is the
///    `NodeKind::Type` `NodeId` for the named type; the canonical
///    signature is the **underlying** function-type's param+result. The
///    Type node is the T1.3 *target* of an `Implements(fn → F)` edge.
///
/// Both shapes are unioned because the consumer (the T1.3 pass) needs a
/// single `NodeId → canonical_signature` map keyed by node identity, and
/// the two `NodeKind` branches are disambiguated at consumption time by
/// inspecting `graph.nodes().get(function_node).kind`.
#[derive(Debug, Clone)]
pub struct GoFunctionSignatureHint {
    /// `NodeId` of either a `NodeKind::Function` (bare-function shape)
    /// or a `NodeKind::Type` (named function-type shape).
    pub function_node: NodeId,
    /// Canonical signature bytes — for function declarations, the
    /// function's own signature; for named function-type declarations,
    /// the underlying `func(...)` signature.
    pub canonical_signature: String,
    /// File in which the declaration was observed.
    pub file: FileId,
}

/// Receiver-expression shape, classifying how the pass resolves the
/// receiver's type at run time.
///
/// `02_DESIGN` §3.2 enumerates these four shapes verbatim; each carries
/// just enough information for the pass to walk the right edges.
#[derive(Debug, Clone)]
pub enum GoReceiverHintKind {
    /// `T.M()` — the receiver is a type-prefixed identifier. The pass
    /// resolves `type_text` to the canonical `Type` `NodeId` via the
    /// by-qualified-name index.
    TypePrefixed {
        /// Source-text form of the type prefix (e.g. `"pkg.Outer"`).
        type_text: String,
    },
    /// `(*T).M()` — pointer-prefixed receiver text. Same resolution
    /// path as `TypePrefixed`, but the pass treats the resolved type
    /// under `MethodSet(*T)` instead of `MethodSet(T)`.
    PointerPrefixed {
        /// Source-text form of the type prefix without the leading `*`
        /// (e.g. `"pkg.Outer"`).
        type_text: String,
    },
    /// `o.M()` where `o` is a local binding (short-var declaration,
    /// `var`-spec, function parameter, method receiver). The pass
    /// resolves the receiver type by walking the `TypeOf` edge from
    /// `binding_local` — the load-bearing path for AC-5
    /// (`var o Outer; o.Greeting()`).
    ///
    /// Cluster B1's eager binding-site materialisation is the
    /// prerequisite that guarantees `binding_local` is non-stub and
    /// carries a `TypeOf` edge.
    LocalIdent {
        /// `NodeId` of the local binding (Variable / Parameter).
        binding_local: NodeId,
    },
    /// `f().M()` — receiver is the return value of another call. The
    /// pass resolves `callee_qn` to the callee's `NodeId`, then follows
    /// the callee's return-type `TypeOf` edge.
    CallReturn {
        /// Qualified name of the callee whose return value is the
        /// receiver.
        callee_qn: String,
    },
}

/// Staging buffer for transactional graph builds.
///
/// Collects all graph mutations in a buffer. Mutations are only
/// applied to the main graph on successful commit.
#[derive(Debug, Default)]
pub struct StagingGraph {
    /// Operations staged for commit.
    operations: Vec<StagingOp>,
    /// Statistics.
    stats: StagingStats,
    /// Mapping from staged node entries to their expected IDs.
    /// Used to map references between staged nodes.
    node_id_map: HashMap<usize, NodeId>,
    /// Current node counter for expected ID assignment.
    next_node_index: usize,
    /// Confidence metadata for the build (optional, language-specific).
    ///
    /// This is used primarily for Rust analysis where confidence can vary
    /// based on available tooling (rust-analyzer vs AST-only).
    confidence: Option<ConfidenceMetadata>,
    /// Macro boundary metadata collected during build.
    ///
    /// Merged into the graph's `NodeMetadataStore` during commit.
    macro_metadata: crate::graph::unified::storage::metadata::NodeMetadataStore,
    /// C indirect-call precision staging payload (Phase A, U10).
    ///
    /// Allocated lazily — non-C plugins leave this `None`, so the per-file
    /// staging buffer's size is unchanged for the 36 non-C language
    /// plugins. The C plugin's Phase 1 walkers (U10) populate the payload
    /// via [`Self::c_indirect_mut`]; Phase 3 commit (U11) drains it via
    /// [`Self::take_c_indirect`] into the workspace-global
    /// [`CIndirectSideTables`](super::super::storage::c_indirect::CIndirectSideTables).
    c_indirect: Option<Box<CIndirectStagingPayload>>,
    /// Go-plugin side-channel hints (Cluster A foundation).
    ///
    /// Populated by the Go plugin's Phase-1 parse via
    /// [`StagingGraph::go_hints_mut`]; drained by the post-Phase-4e
    /// `pass_go_method_set_satisfaction` after Phase 3 commit has
    /// remapped the embedded staging-local `NodeId`s / `StringId`s.
    /// Plugins for other languages leave this empty.
    go_hints: GoHints,
}

impl StagingGraph {
    /// Create a new empty staging graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with pre-allocated capacity.
    #[must_use]
    pub fn with_capacity(nodes: usize, edges: usize) -> Self {
        Self {
            operations: Vec::with_capacity(nodes + edges),
            stats: StagingStats::default(),
            node_id_map: HashMap::with_capacity(nodes),
            next_node_index: 0,
            confidence: None,
            macro_metadata: crate::graph::unified::storage::metadata::NodeMetadataStore::new(),
            c_indirect: None,
            go_hints: GoHints::default(),
        }
    }

    /// Shared borrow of the Go-plugin side-channel hint buffers
    /// (Cluster A foundation).
    ///
    /// Returns the empty default on non-Go staging graphs. The pass
    /// reads through this accessor after Phase 3 commit has remapped
    /// the embedded staging-local identities.
    #[must_use]
    pub fn go_hints(&self) -> &GoHints {
        &self.go_hints
    }

    /// Mutable borrow of the Go-plugin side-channel hint buffers
    /// (Cluster A foundation).
    ///
    /// Used by the Go plugin's Phase-1 parser to push
    /// [`GoEmbeddingHint`] / [`GoNamedTypeConversionHint`] /
    /// [`GoReceiverCallHint`] entries during AST traversal.
    pub fn go_hints_mut(&mut self) -> &mut GoHints {
        &mut self.go_hints
    }

    /// Approximate memory footprint of this staging buffer in bytes.
    ///
    /// Counts operations and `HashMap` entries. Excludes allocator overhead.
    /// Used for build-time memory instrumentation during parallel indexing.
    #[must_use]
    pub fn estimated_byte_size(&self) -> usize {
        let ops_bytes = self.operations.len() * std::mem::size_of::<StagingOp>();
        let map_bytes =
            self.node_id_map.len() * (std::mem::size_of::<usize>() + std::mem::size_of::<NodeId>());

        // Account for heap allocations inside each StagingOp variant.
        let heap_bytes: usize = self
            .operations
            .iter()
            .map(|op| match op {
                StagingOp::AddEdge { spans, .. } => {
                    spans.capacity() * std::mem::size_of::<crate::graph::node::Span>()
                }
                StagingOp::RegisterFile { path, .. } => path.capacity(),
                StagingOp::InternString { value, .. } => value.capacity(),
                StagingOp::AddNode { .. } => 0, // NodeEntry is all stack-allocated
            })
            .sum();

        ops_bytes + map_bytes + heap_bytes
    }

    /// Stage a node addition.
    ///
    /// Returns the expected `NodeId` that will be assigned on commit.
    /// This allows staging edges that reference nodes added in the same batch.
    ///
    /// # Panics
    /// Panics if the staged node count exceeds `u32::MAX`.
    pub fn add_node(&mut self, entry: NodeEntry) -> NodeId {
        // Generate expected ID (will be reconciled during commit)
        let node_index = u32::try_from(self.next_node_index).expect("staging node id overflow");
        let expected_id = NodeId::new(node_index, 1);
        self.node_id_map.insert(self.next_node_index, expected_id);
        self.next_node_index += 1;

        self.operations.push(StagingOp::AddNode {
            entry,
            expected_id: Some(expected_id),
        });
        self.stats.nodes_staged += 1;

        expected_id
    }

    /// Update an existing staged node entry with additional metadata.
    ///
    /// Returns true if the node was found and updated.
    pub fn update_node_entry(&mut self, node_id: NodeId, update: &NodeMetadataUpdate) -> bool {
        for op in &mut self.operations {
            if let StagingOp::AddNode {
                entry,
                expected_id: Some(expected_id),
            } = op
            {
                if *expected_id != node_id {
                    continue;
                }

                apply_node_metadata(entry, update);
                return true;
            }
        }

        false
    }

    /// Stage an edge addition.
    pub fn add_edge(&mut self, source: NodeId, target: NodeId, kind: EdgeKind, file: FileId) {
        self.add_edge_with_spans(source, target, kind, file, Vec::new());
    }

    /// Stage an edge with source span information.
    ///
    /// The spans represent locations of the edge in source code (e.g., call sites).
    pub fn add_edge_with_spans(
        &mut self,
        source: NodeId,
        target: NodeId,
        kind: EdgeKind,
        file: FileId,
        spans: Vec<crate::graph::node::Span>,
    ) {
        self.operations.push(StagingOp::AddEdge {
            source,
            target,
            kind,
            file,
            spans,
        });
        self.stats.edges_staged += 1;
    }

    /// Stage multiple edges from `PendingEdge` structs.
    ///
    /// Preserves span data from each `PendingEdge`.
    pub fn add_edges(&mut self, edges: &[PendingEdge]) {
        for edge in edges {
            self.add_edge_with_spans(
                edge.source,
                edge.target,
                edge.kind.clone(),
                edge.file,
                edge.spans.clone(),
            );
        }
    }

    /// Stage a file registration.
    pub fn register_file(&mut self, path: String, expected_id: FileId) {
        self.operations
            .push(StagingOp::RegisterFile { path, expected_id });
        self.stats.files_registered += 1;
    }

    /// Stage a string interning with a local `StringId`.
    ///
    /// The `local_id` is the staging-local ID allocated by `GraphBuildHelper`.
    /// During `commit_strings()`, this local ID will be mapped to a global ID
    /// in the `StringInterner`, and the mapping is returned for remapping
    /// staged operations.
    pub fn intern_string(&mut self, local_id: crate::graph::unified::StringId, value: String) {
        self.operations
            .push(StagingOp::InternString { local_id, value });
        self.stats.strings_interned += 1;
    }

    /// Get staging statistics.
    #[must_use]
    pub fn stats(&self) -> &StagingStats {
        &self.stats
    }

    /// Get the number of staged operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Check if the staging buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Number of nodes staged, as `u32`.
    ///
    /// Used by the parallel commit pipeline for prefix-sum range assignment.
    ///
    /// # Panics
    ///
    /// Panics if the count exceeds `u32::MAX` (impossible for a single file).
    #[must_use]
    pub fn node_count_u32(&self) -> u32 {
        u32::try_from(self.stats.nodes_staged).expect("single file node count exceeds u32::MAX")
    }

    /// Number of strings interned, as `u32`.
    ///
    /// Used by the parallel commit pipeline for prefix-sum range assignment.
    ///
    /// # Panics
    ///
    /// Panics if the count exceeds `u32::MAX` (impossible for a single file).
    #[must_use]
    pub fn string_count_u32(&self) -> u32 {
        u32::try_from(self.stats.strings_interned)
            .expect("single file string count exceeds u32::MAX")
    }

    /// Number of edges staged, as `u32`.
    ///
    /// Used by the parallel commit pipeline for prefix-sum range assignment.
    ///
    /// # Panics
    ///
    /// Panics if the count exceeds `u32::MAX` (impossible for a single file).
    #[must_use]
    pub fn edge_count_u32(&self) -> u32 {
        u32::try_from(self.stats.edges_staged).expect("single file edge count exceeds u32::MAX")
    }

    /// Set confidence metadata for this staging graph.
    ///
    /// This is typically called by language plugins that compute confidence
    /// during graph building (e.g., Rust plugin tracking rust-analyzer availability).
    pub fn set_confidence(&mut self, confidence: ConfidenceMetadata) {
        self.confidence = Some(confidence);
    }

    /// Get confidence metadata.
    ///
    /// Returns `None` if no confidence metadata was set.
    #[must_use]
    pub fn confidence(&self) -> Option<&ConfidenceMetadata> {
        self.confidence.as_ref()
    }

    /// Take confidence metadata, consuming it from the staging graph.
    ///
    /// This is useful when transferring confidence metadata to the committed graph.
    #[must_use]
    pub fn take_confidence(&mut self) -> Option<ConfidenceMetadata> {
        self.confidence.take()
    }

    /// Merge macro boundary metadata into this staging graph.
    ///
    /// Called by the Rust plugin after macro boundary analysis to attach
    /// metadata (cfg conditions, proc-macro classifications, etc.) to nodes.
    pub fn merge_macro_metadata(
        &mut self,
        #[allow(clippy::needless_continue)] // Continue at end of loop for clarity
        metadata: &crate::graph::unified::storage::metadata::NodeMetadataStore,
    ) {
        self.macro_metadata.merge(metadata);
    }

    /// Take macro metadata, consuming it from the staging graph.
    #[must_use]
    pub fn take_macro_metadata(
        &mut self,
    ) -> crate::graph::unified::storage::metadata::NodeMetadataStore {
        std::mem::take(&mut self.macro_metadata)
    }

    /// Read-only check whether a staging-local `NodeId` already carries a
    /// `NodeMetadata::Synthetic` marker in this staging graph's metadata
    /// store.
    ///
    /// Plugins that emit synthetic placeholders (e.g. the Go plugin's
    /// `add_synthetic_variable` at `sqry-lang-go/src/relations/graph_builder.rs`)
    /// merge the marker into `macro_metadata` immediately after staging the
    /// node. Per-file post-passes (e.g. T3.8's
    /// `stamp_cfg_condition_for_file` per `02_DESIGN` §4.3.d) consult this
    /// predicate to skip synthetic nodes before stamping language-agnostic
    /// `cfg_condition` metadata — otherwise the stamp would overwrite the
    /// synthetic marker via `NodeMetadataStore::insert_metadata`'s
    /// overwrite semantics and defeat suppression.
    ///
    /// This is a read-only proxy onto `NodeMetadataStore::is_synthetic`;
    /// it never mutates the staging graph.
    #[must_use]
    pub fn is_node_synthetic(&self, node_id: crate::graph::unified::node::id::NodeId) -> bool {
        self.macro_metadata.is_synthetic(node_id)
    }

    /// Iterator over every staging-local `NodeId` this staging graph has
    /// allocated via `add_node`, in allocation order.
    ///
    /// IDs are sequential `NodeId::new(0, 1) .. NodeId::new(N, 1)` where
    /// `N` is `node_count_u32()` (see `add_node` at `:355` for the
    /// invariant). Useful for per-file post-passes that need to walk
    /// every staged node (e.g. T3.8's `stamp_cfg_condition_for_file`
    /// per `02_DESIGN` §4.3.d).
    pub fn staged_node_ids(
        &self,
    ) -> impl Iterator<Item = crate::graph::unified::node::id::NodeId> + '_ {
        let n = self.node_count_u32();
        (0..n).map(|i| crate::graph::unified::node::id::NodeId::new(i, 1))
    }

    /// Immutable accessor for the C indirect-call staging payload, if any.
    ///
    /// Returns `None` for non-C plugins (the default) or for C files that
    /// produced no indirect-call instrumentation (e.g. empty translation
    /// units). Exposed primarily for `sqry-lang-c` integration tests that
    /// need to inspect the per-file payload after `build_graph` returns.
    /// Phase 3 commit (U11) consumes the payload via
    /// [`Self::take_c_indirect`].
    #[must_use]
    pub fn c_indirect(&self) -> Option<&CIndirectStagingPayload> {
        self.c_indirect.as_deref()
    }

    /// Mutable accessor for the C indirect-call staging payload, lazily
    /// allocating it on first access.
    ///
    /// Non-C plugins never call this — the parent `Option` stays `None`,
    /// keeping `StagingGraph` size unchanged for the other 36 language
    /// plugins. The C plugin's Phase 1 walkers (U10) call this via
    /// [`super::helper::GraphBuildHelper::c_indirect_mut`] to push
    /// address-taken names, indirect callsites, binding entries, and
    /// struct function-pointer signatures.
    #[must_use]
    pub fn c_indirect_mut(&mut self) -> &mut CIndirectStagingPayload {
        self.c_indirect
            .get_or_insert_with(|| Box::new(CIndirectStagingPayload::default()))
    }

    /// Take the C indirect-call staging payload, leaving `None` behind.
    ///
    /// Used by Phase 3 commit (U11) to drain the per-file payload into the
    /// workspace-global
    /// [`CIndirectSideTables`](super::super::storage::c_indirect::CIndirectSideTables).
    #[must_use]
    pub fn take_c_indirect(&mut self) -> Option<Box<CIndirectStagingPayload>> {
        self.c_indirect.take()
    }

    /// Commit all staged operations to the given arena.
    ///
    /// This is a simplified commit that only handles node additions.
    /// For full graph integration, use `commit_to_graph`.
    ///
    /// Returns mapping from expected IDs to actual allocated IDs.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::NodeAllocationFailed` if the arena rejects a node allocation.
    pub fn commit_nodes(
        &mut self,
        arena: &mut NodeArena,
    ) -> Result<HashMap<NodeId, NodeId>, StagingError> {
        let mut id_mapping = HashMap::new();

        for op in &self.operations {
            if let StagingOp::AddNode { entry, expected_id } = op {
                let actual_id =
                    arena
                        .alloc(entry.clone())
                        .map_err(|e| StagingError::NodeAllocationFailed {
                            reason: format!("{e:?}"),
                        })?;

                if let Some(expected) = expected_id {
                    id_mapping.insert(*expected, actual_id);
                }
            }
        }

        Ok(id_mapping)
    }

    /// Commit strings to the interner and return a local→global `StringId` remap table.
    ///
    /// This method processes all `InternString` operations, interns each string
    /// in the global `StringInterner`, and builds a mapping from local (staging)
    /// `StringIds` to global (interner) `StringIds`.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::DuplicateLocalStringId` if the same `local_id` appears
    /// twice in the staged operations (indicates misuse of the staging API).
    ///
    /// Returns `StagingError::InternCapacityExhausted` if the `StringInterner`
    /// cannot allocate any more string IDs.
    ///
    /// # Commit Flow
    ///
    /// The proper commit flow is:
    /// 1. `commit_strings()?` - Get the remap table
    /// 2. `apply_string_remap(&remap)` - Update all staged ops with global IDs
    /// 3. `commit_nodes()` - Commit nodes to arena
    /// 4. Commit edges using remapped node IDs
    pub fn commit_strings(
        &self,
        strings: &mut StringInterner,
    ) -> Result<HashMap<StringId, StringId>, StagingError> {
        let mut remap = HashMap::new();
        let mut seen_local_ids = HashSet::new();

        for op in &self.operations {
            if let StagingOp::InternString { local_id, value } = op {
                if !local_id.is_local() {
                    return Err(StagingError::NonLocalInternStringId { id: *local_id });
                }
                // Check for duplicate local_ids (indicates misuse)
                if !seen_local_ids.insert(*local_id) {
                    return Err(StagingError::DuplicateLocalStringId {
                        local_id: *local_id,
                    });
                }

                // Intern the string and get the global ID
                let global_id = strings
                    .intern(value)
                    .map_err(|_| StagingError::InternCapacityExhausted)?;

                // Record the mapping from local to global
                remap.insert(*local_id, global_id);
            }
        }

        Ok(remap)
    }

    /// Apply a `StringId` remap to all staged operations.
    ///
    /// This method updates all `StringIds` in staged `AddNode` and `AddEdge` operations
    /// from local (staging) IDs to global (interner) IDs.
    ///
    /// # `StringId` Carriers Updated
    ///
    /// **`NodeEntry` fields**:
    /// - `name` (required)
    /// - `signature`, `doc`, `qualified_name`, `visibility` (optional)
    ///
    /// **`EdgeKind` fields**:
    /// - `Imports { alias }`, `Exports { alias }`, `DbQuery { table }`, `HttpRequest { url }`
    /// - `GrpcCall { service, method }`, `GraphQLOperation { operation }`, `ProcessExec { command }`
    /// - `MessageQueue { topic, protocol: MqProtocol::Other(StringId) }`
    /// - `WebSocket { event }`, `FileIpc { path_pattern }`, `ProtocolCall { protocol, metadata }`
    ///
    /// # Panics
    ///
    /// This method does NOT panic if a *global* `StringId` is not found in the remap table.
    /// Global IDs are left unchanged.
    ///
    /// If a *staging-local* `StringId` is encountered and is missing from the remap table,
    /// this method returns `StagingError::UnmappedLocalStringId`.
    ///
    /// # Errors
    ///
    /// Returns `StagingError::UnmappedLocalStringId` if a staging-local `StringId` has no entry
    /// in the provided remap table.
    pub fn apply_string_remap(
        &mut self,
        remap: &HashMap<StringId, StringId>,
    ) -> Result<(), StagingError> {
        for op in &mut self.operations {
            match op {
                StagingOp::AddNode { entry, .. } => {
                    Self::remap_node_entry(entry, remap)?;
                }
                StagingOp::AddEdge { kind, .. } => {
                    Self::remap_edge_kind(kind, remap)?;
                }
                StagingOp::InternString { .. } | StagingOp::RegisterFile { .. } => {
                    // No StringIds to remap in these ops
                }
            }
        }
        Ok(())
    }

    /// Apply a per-file `FileId` to all staged operations.
    ///
    /// The unified graph build pipeline constructs a fresh `StagingGraph` per file.
    /// Language `GraphBuilder` implementations typically allocate nodes/edges before the
    /// target `FileId` is known, so staged `NodeEntry.file` / `StagingOp::AddEdge.file`
    /// values may be a placeholder. This helper normalizes all staged operations to use
    /// the committed graph's `file_id`.
    pub fn apply_file_id(&mut self, file_id: FileId) {
        for op in &mut self.operations {
            match op {
                StagingOp::AddNode { entry, .. } => {
                    entry.file = file_id;
                }
                StagingOp::AddEdge { file, .. } => {
                    *file = file_id;
                }
                StagingOp::RegisterFile { expected_id, .. } => {
                    *expected_id = file_id;
                }
                StagingOp::InternString { .. } => {}
            }
        }
    }

    /// Helper: remap `StringIds` in a `NodeEntry`.
    fn remap_node_entry(
        entry: &mut NodeEntry,
        remap: &HashMap<StringId, StringId>,
    ) -> Result<(), StagingError> {
        // Required field: name
        Self::remap_required_string_id(&mut entry.name, remap, "NodeEntry.name")?;

        // Optional fields
        Self::remap_optional_string_id(&mut entry.signature, remap, "NodeEntry.signature")?;
        Self::remap_optional_string_id(&mut entry.doc, remap, "NodeEntry.doc")?;
        Self::remap_optional_string_id(
            &mut entry.qualified_name,
            remap,
            "NodeEntry.qualified_name",
        )?;
        Self::remap_optional_string_id(&mut entry.visibility, remap, "NodeEntry.visibility")?;

        Ok(())
    }

    /// Helper: remap `StringIds` in an `EdgeKind`.
    #[allow(
        clippy::too_many_lines,
        reason = "Long match keeps edge remap logic centralized and auditable"
    )]
    fn remap_edge_kind(
        kind: &mut EdgeKind,
        remap: &HashMap<StringId, StringId>,
    ) -> Result<(), StagingError> {
        match kind {
            EdgeKind::Imports { alias, .. } => {
                Self::remap_optional_string_id(alias, remap, "EdgeKind::Imports.alias")?;
            }
            EdgeKind::Exports { alias, .. } => {
                Self::remap_optional_string_id(alias, remap, "EdgeKind::Exports.alias")?;
            }
            EdgeKind::DbQuery { table, .. } => {
                Self::remap_optional_string_id(table, remap, "EdgeKind::DbQuery.table")?;
            }
            EdgeKind::TableRead { table_name, schema } => {
                Self::remap_required_string_id(
                    table_name,
                    remap,
                    "EdgeKind::TableRead.table_name",
                )?;
                Self::remap_optional_string_id(schema, remap, "EdgeKind::TableRead.schema")?;
            }
            EdgeKind::TableWrite {
                table_name, schema, ..
            } => {
                Self::remap_required_string_id(
                    table_name,
                    remap,
                    "EdgeKind::TableWrite.table_name",
                )?;
                Self::remap_optional_string_id(schema, remap, "EdgeKind::TableWrite.schema")?;
            }
            EdgeKind::TriggeredBy {
                trigger_name,
                schema,
            } => {
                Self::remap_required_string_id(
                    trigger_name,
                    remap,
                    "EdgeKind::TriggeredBy.trigger_name",
                )?;
                Self::remap_optional_string_id(schema, remap, "EdgeKind::TriggeredBy.schema")?;
            }
            EdgeKind::HttpRequest { url, .. } => {
                Self::remap_optional_string_id(url, remap, "EdgeKind::HttpRequest.url")?;
            }
            EdgeKind::GrpcCall { service, method } => {
                Self::remap_required_string_id(service, remap, "EdgeKind::GrpcCall.service")?;
                Self::remap_required_string_id(method, remap, "EdgeKind::GrpcCall.method")?;
            }
            EdgeKind::GraphQLOperation { operation } => {
                Self::remap_required_string_id(
                    operation,
                    remap,
                    "EdgeKind::GraphQLOperation.operation",
                )?;
            }
            EdgeKind::ProcessExec { command } => {
                Self::remap_required_string_id(command, remap, "EdgeKind::ProcessExec.command")?;
            }
            EdgeKind::MessageQueue { protocol, topic } => {
                // Remap topic
                Self::remap_optional_string_id(topic, remap, "EdgeKind::MessageQueue.topic")?;
                // Remap MqProtocol::Other(StringId) if present
                if let MqProtocol::Other(string_id) = protocol {
                    Self::remap_required_string_id(string_id, remap, "MqProtocol::Other")?;
                }
            }
            EdgeKind::WebSocket { event } => {
                Self::remap_optional_string_id(event, remap, "EdgeKind::WebSocket.event")?;
            }
            EdgeKind::FileIpc { path_pattern } => {
                Self::remap_optional_string_id(
                    path_pattern,
                    remap,
                    "EdgeKind::FileIpc.path_pattern",
                )?;
            }
            EdgeKind::ProtocolCall { protocol, metadata } => {
                Self::remap_required_string_id(protocol, remap, "EdgeKind::ProtocolCall.protocol")?;
                Self::remap_optional_string_id(metadata, remap, "EdgeKind::ProtocolCall.metadata")?;
            }
            // Rust-specific: TraitMethodBinding has StringId fields
            EdgeKind::TraitMethodBinding {
                trait_name,
                impl_type,
                ..
            } => {
                Self::remap_required_string_id(
                    trait_name,
                    remap,
                    "EdgeKind::TraitMethodBinding.trait_name",
                )?;
                Self::remap_required_string_id(
                    impl_type,
                    remap,
                    "EdgeKind::TraitMethodBinding.impl_type",
                )?;
            }
            EdgeKind::TypeOf { name, .. } => {
                Self::remap_optional_string_id(name, remap, "EdgeKind::TypeOf.name")?;
            }
            // T2.5: each TypeArg.name StringId is baked at parse time and must
            // be rewritten through the local→global remap, else it dangles
            // after the global string-table dedup.
            EdgeKind::Instantiates { type_args, .. } => {
                for ta in type_args.iter_mut() {
                    Self::remap_required_string_id(
                        &mut ta.name,
                        remap,
                        "EdgeKind::Instantiates.type_args[].name",
                    )?;
                }
            }
            // Variants without StringId fields
            EdgeKind::Defines
            | EdgeKind::Contains
            | EdgeKind::Calls { .. }
            | EdgeKind::References
            | EdgeKind::Inherits
            | EdgeKind::Implements
            | EdgeKind::FfiCall { .. }
            | EdgeKind::WebAssemblyCall
            | EdgeKind::LifetimeConstraint { .. }
            | EdgeKind::MacroExpansion { .. }
            | EdgeKind::GenericBound
            | EdgeKind::AnnotatedWith
            | EdgeKind::AnnotationParam
            | EdgeKind::LambdaCaptures
            | EdgeKind::ModuleExports
            | EdgeKind::ModuleRequires
            | EdgeKind::ModuleOpens
            | EdgeKind::ModuleProvides
            | EdgeKind::TypeArgument
            | EdgeKind::ExtensionReceiver
            | EdgeKind::CompanionOf
            | EdgeKind::SealedPermit
            // T3 Wraps carries WrapKind (Copy) + Option<u16>; no StringId fields.
            | EdgeKind::Wraps { .. }
            // T2.4 ChannelPeer carries only Copy enums; no StringId fields.
            | EdgeKind::ChannelPeer { .. } => {
                // No StringIds to remap
            }
        }
        Ok(())
    }

    fn remap_required_string_id(
        id: &mut StringId,
        remap: &HashMap<StringId, StringId>,
        carrier: &'static str,
    ) -> Result<(), StagingError> {
        if id.is_invalid() {
            return Err(StagingError::InvalidRequiredStringId { id: *id, carrier });
        }
        if id.is_local() {
            let Some(&global_id) = remap.get(id) else {
                return Err(StagingError::UnmappedLocalStringId {
                    local_id: *id,
                    carrier,
                });
            };
            *id = global_id;
        }
        Ok(())
    }

    /// Helper: remap an Option<StringId> in place.
    fn remap_optional_string_id(
        opt: &mut Option<StringId>,
        remap: &HashMap<StringId, StringId>,
        carrier: &'static str,
    ) -> Result<(), StagingError> {
        if let Some(id) = opt
            && id.is_local()
        {
            let Some(&global_id) = remap.get(id) else {
                return Err(StagingError::UnmappedLocalStringId {
                    local_id: *id,
                    carrier,
                });
            };
            *opt = Some(global_id);
        }
        Ok(())
    }

    /// Get all staged edges, remapping IDs using the provided mapping.
    ///
    /// Used after `commit_nodes` to get edges with actual (not expected) IDs.
    #[must_use]
    pub fn get_remapped_edges(&self, id_mapping: &HashMap<NodeId, NodeId>) -> Vec<PendingEdge> {
        self.operations
            .iter()
            .filter_map(|op| {
                if let StagingOp::AddEdge {
                    source,
                    target,
                    kind,
                    file,
                    spans,
                } = op
                {
                    let actual_source = id_mapping.get(source).copied().unwrap_or(*source);
                    let actual_target = id_mapping.get(target).copied().unwrap_or(*target);
                    Some(PendingEdge {
                        source: actual_source,
                        target: actual_target,
                        kind: kind.clone(),
                        file: *file,
                        spans: spans.clone(),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Rollback all staged operations (discard them).
    pub fn rollback(&mut self) {
        self.operations.clear();
        self.node_id_map.clear();
        self.next_node_index = 0;
        self.stats = StagingStats::default();
        self.confidence = None;
        self.c_indirect = None;
    }

    /// Clear the staging buffer after successful commit.
    pub fn clear(&mut self) {
        self.rollback();
    }

    /// Attach body hashes (and, when a shape context is supplied, identifier-blind
    /// shape descriptors) to staged nodes.
    ///
    /// This should be called after building the staging graph but before committing.
    /// It iterates through all staged `AddNode` operations and computes body hashes
    /// for nodes with hashable kinds (Function, Method, Class, Struct, Enum,
    /// Interface, Trait, Module).
    ///
    /// # Arguments
    ///
    /// * `content` - The source file content as bytes (the body-hash coordinate
    ///   system, the raw bytes the index records).
    /// * `shape` - Optional shape-compute context (parsed tree + per-language
    ///   [`ShapeMapping`](crate::graph::unified::build::shape::ShapeMapping) + visit
    ///   budget). When `Some`, a [`ShapeDescriptor`](crate::graph::unified::storage::shape::ShapeDescriptor)
    ///   is computed for each Function/Method node with a valid body span and inserted
    ///   into this staging graph's `NodeMetadataStore` (the one
    ///   [`take_macro_metadata`](Self::take_macro_metadata) drains), keyed by the
    ///   staging-local `NodeId`, so descriptors ride the existing
    ///   take -> rekey -> merge pipeline. When `None`, only body hashes are computed
    ///   (the pre-feature behaviour, used by non-index build paths and languages with
    ///   no mapping yet).
    ///
    /// # Notes
    ///
    /// - Only nodes with valid body spans (`start_line` > 0, end > start) are hashed.
    ///   Shape descriptors share that exact gate, so a `{0,0}` data-quality span
    ///   suppresses both identically (SPEC §7).
    /// - Bodies smaller than 4 bytes are not hashed to avoid trivial matches; the
    ///   shape walker independently emits an explicit `unhashable` marker for tiny
    ///   bodies (it never silently drops one).
    pub fn attach_body_hashes(&mut self, content: &[u8], shape: Option<&ShapeAttachCtx<'_>>) {
        use super::body_hash::{build_line_offsets, compute_node_body_hash, has_valid_body_span};
        use crate::graph::unified::node::kind::NodeKind;

        let line_offsets = build_line_offsets(content);

        // Shape descriptors must land in the SAME staging `NodeMetadataStore` that
        // `take_macro_metadata()` drains, so they ride the existing
        // take -> rekey_staging_metadata_to_arena -> merge pipeline. The operations
        // walk borrows `self.operations` mutably (to assign `body_hash`), so collect
        // here and flush into `self.macro_metadata` once that borrow ends.
        let mut pending_descriptors: Vec<(
            NodeId,
            crate::graph::unified::storage::shape::ShapeDescriptor,
        )> = Vec::new();

        for op in &mut self.operations {
            if let StagingOp::AddNode { entry, expected_id } = op {
                if entry.body_hash.is_none() {
                    entry.body_hash = compute_node_body_hash(content, entry, &line_offsets);
                }
                // Sibling computation: identifier-blind shape descriptor for
                // Function/Method bodies, gated on the same span-validity contract
                // as body_hash above.
                if let Some(ctx) = shape
                    && let Some(node_id) = *expected_id
                    && matches!(entry.kind, NodeKind::Function | NodeKind::Method)
                    && has_valid_body_span(entry)
                    && let Some(descriptor) = ctx.descriptor_for(entry)
                {
                    pending_descriptors.push((node_id, descriptor));
                }
            }
        }

        for (node_id, descriptor) in pending_descriptors {
            self.macro_metadata
                .insert_shape_descriptor(node_id, descriptor);
        }
    }

    /// Get an iterator over staged operations for relation extraction.
    ///
    /// This provides read-only access to the operations for extracting
    /// call/import edges during index building.
    #[must_use]
    pub fn operations(&self) -> &[StagingOp] {
        &self.operations
    }

    /// Iterate over all staged `AddNode` operations as typed references.
    ///
    /// This filters the operation buffer to yield only node additions,
    /// providing zero-copy access to the `NodeEntry` and its expected `NodeId`.
    pub fn nodes(&self) -> impl Iterator<Item = StagedNodeRef<'_>> {
        self.operations.iter().filter_map(|op| {
            if let StagingOp::AddNode { entry, expected_id } = op {
                Some(StagedNodeRef {
                    entry,
                    expected_id: *expected_id,
                })
            } else {
                None
            }
        })
    }

    /// Iterate over all staged `AddEdge` operations as typed references.
    ///
    /// This filters the operation buffer to yield only edge additions,
    /// providing zero-copy access to source/target IDs, edge kind, and spans.
    pub fn edges(&self) -> impl Iterator<Item = StagedEdgeRef<'_>> {
        self.operations.iter().filter_map(|op| {
            if let StagingOp::AddEdge {
                source,
                target,
                kind,
                file,
                spans,
            } = op
            {
                Some(StagedEdgeRef {
                    source: *source,
                    target: *target,
                    kind,
                    file: *file,
                    spans,
                })
            } else {
                None
            }
        })
    }

    /// Return the number of staged nodes (O(1) from stats).
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.stats.nodes_staged
    }

    /// Return the number of staged edges (O(1) from stats).
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.stats.edges_staged
    }

    /// Resolve a staging-local `StringId` to its interned string value.
    ///
    /// Performs a linear scan of `InternString` operations. Intended for
    /// test and debug use — not optimized for hot paths.
    ///
    /// Returns `None` if the `StringId` is not found among staged strings.
    #[must_use]
    pub fn resolve_local_string(&self, id: StringId) -> Option<&str> {
        self.operations.iter().find_map(|op| {
            if let StagingOp::InternString { local_id, value } = op
                && *local_id == id
            {
                Some(value.as_str())
            } else {
                None
            }
        })
    }

    /// Resolve a `NodeEntry`'s canonical/structural name from staged `InternString` operations.
    ///
    /// Prefers `qualified_name` if set, falling back to `name`.
    /// Performs a linear scan — intended for test/debug use only.
    ///
    /// Returns `None` if the name `StringId` is not found among staged strings.
    #[must_use]
    pub fn resolve_node_canonical_name(&self, entry: &NodeEntry) -> Option<&str> {
        let name_id = entry.qualified_name.unwrap_or(entry.name);
        self.resolve_local_string(name_id)
    }

    /// Resolve a `NodeEntry`'s native/display name from staged `InternString` operations.
    ///
    /// When a canonical `qualified_name` exists, this converts it back into the
    /// language-native display form. Otherwise it falls back to the stored `name`.
    /// Performs a linear scan — intended for test/debug use only.
    #[must_use]
    pub fn resolve_node_display_name(
        &self,
        language: Language,
        entry: &NodeEntry,
    ) -> Option<String> {
        entry
            .qualified_name
            .and_then(|qualified_name_id| self.resolve_local_string(qualified_name_id))
            .map(|qualified_name| {
                display_graph_qualified_name(language, qualified_name, entry.kind, entry.is_static)
            })
            .or_else(|| self.resolve_local_string(entry.name).map(str::to_owned))
    }

    /// Compatibility wrapper for canonical/structural node name resolution.
    ///
    /// Prefer `resolve_node_canonical_name()` for graph identity checks and
    /// `resolve_node_display_name()` for user-facing/native language assertions.
    #[must_use]
    pub fn resolve_node_name(&self, entry: &NodeEntry) -> Option<&str> {
        self.resolve_node_canonical_name(entry)
    }
}

/// Inputs the identifier-blind shape walker needs that
/// [`StagingGraph::attach_body_hashes`] does not otherwise carry.
///
/// Built once per file in `parse_file` after the file's plugin resolves a
/// [`ShapeMapping`](crate::graph::unified::build::shape::ShapeMapping). It holds
/// the parsed tree (so a Function/Method [`NodeEntry`] span can be resolved back to
/// its subtree), the source the tree was parsed from (the coordinate system the
/// recorded spans live in, which may differ from the raw body-hash bytes when a
/// plugin preprocesses), the per-language mapping, and the visit budget. `None` at
/// the call site means "compute body hashes only" — the behaviour before this
/// feature, and the path for any language without a mapping yet.
pub struct ShapeAttachCtx<'a> {
    tree: &'a tree_sitter::Tree,
    src: &'a [u8],
    mapping: &'a dyn crate::graph::unified::build::shape::ShapeMapping,
    budget: crate::graph::unified::build::shape::ShapeBudget,
    /// Line-start byte offsets over `src`, computed once so each `descriptor_for`
    /// converts a recorded line/column span to bytes without re-scanning.
    line_offsets: Vec<usize>,
}

impl<'a> ShapeAttachCtx<'a> {
    /// Build a context with the default visit budget.
    #[must_use]
    pub fn new(
        tree: &'a tree_sitter::Tree,
        src: &'a [u8],
        mapping: &'a dyn crate::graph::unified::build::shape::ShapeMapping,
    ) -> Self {
        Self {
            tree,
            src,
            mapping,
            budget: crate::graph::unified::build::shape::ShapeBudget::default(),
            line_offsets: crate::graph::unified::build::body_hash::build_line_offsets(src),
        }
    }

    /// Resolve the tree node whose byte span exactly matches `entry` and compute
    /// its identifier-blind descriptor.
    ///
    /// The recorded span is first converted to a byte range with the same
    /// `resolve_body_span` `body_hash` uses. Going through the byte range (rather than
    /// feeding the recorded line/column straight to `descendant_for_point_range`) is
    /// what makes this work across the whole plugin set: most plugins record true
    /// `(row, column)`, but several (php, perl, r, elixir, plsql, ...) encode the
    /// span as line 1 plus an absolute byte offset in the column. `resolve_body_span`
    /// maps both conventions to the correct bytes via `line_offsets`, exactly as it
    /// does for `body_hash`, so the two seams stay in lock-step.
    ///
    /// `descendant_for_byte_range` returns the SMALLEST node spanning the range,
    /// which for a recorded function/method span is the function node itself. A
    /// non-exact match means the recorded span does not correspond to a real
    /// subtree (e.g. a genuine preprocessing skew where the recorded coordinates
    /// belong to a different content buffer), so we return `None` rather than
    /// fingerprint the wrong node, the same conservative stance `body_hash` takes for
    /// an invalid span.
    fn descriptor_for(
        &self,
        entry: &NodeEntry,
    ) -> Option<crate::graph::unified::storage::shape::ShapeDescriptor> {
        let (start_byte, end_byte) = crate::graph::unified::build::body_hash::resolve_body_span(
            &self.line_offsets,
            entry.start_line,
            entry.start_column,
            entry.end_line,
            entry.end_column,
            self.src.len(),
        )?;
        let node = self
            .tree
            .root_node()
            .descendant_for_byte_range(start_byte, end_byte)?;
        if node.start_byte() != start_byte || node.end_byte() != end_byte {
            return None;
        }
        Some(
            crate::graph::unified::build::shape::compute_shape_descriptor(
                node,
                self.src,
                self.mapping,
                &self.budget,
            ),
        )
    }
}

/// Zero-copy reference to a staged node addition.
#[derive(Debug, Clone, Copy)]
pub struct StagedNodeRef<'a> {
    /// The node entry being staged.
    pub entry: &'a NodeEntry,
    /// The expected `NodeId` after allocation (if assigned).
    pub expected_id: Option<NodeId>,
}

/// Zero-copy reference to a staged edge addition.
#[derive(Debug, Clone)]
pub struct StagedEdgeRef<'a> {
    /// Source node ID.
    pub source: NodeId,
    /// Target node ID.
    pub target: NodeId,
    /// Edge kind.
    pub kind: &'a EdgeKind,
    /// File containing the edge.
    pub file: FileId,
    /// Source spans of the edge.
    pub spans: &'a [crate::graph::node::Span],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::unified::StringId;
    use crate::graph::unified::node::NodeKind;

    fn create_test_entry(name_id: StringId, file_id: FileId) -> NodeEntry {
        NodeEntry::new(NodeKind::Function, name_id, file_id)
    }

    #[test]
    fn test_staging_add_node() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);
        let entry = create_test_entry(name_id, file_id);

        let expected_id = staging.add_node(entry);

        assert_eq!(staging.stats().nodes_staged, 1);
        assert_eq!(staging.len(), 1);
        assert_eq!(expected_id.index(), 0);
    }

    #[test]
    fn test_staging_add_edge() {
        let mut staging = StagingGraph::new();
        let source = NodeId::new(0, 1);
        let target = NodeId::new(1, 1);
        let file_id = FileId::new(0);

        staging.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        assert_eq!(staging.stats().edges_staged, 1);
        assert_eq!(staging.len(), 1);
    }

    #[test]
    fn test_staging_commit_nodes() {
        let mut staging = StagingGraph::new();
        let mut arena = NodeArena::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        let entry1 = create_test_entry(name_id, file_id);
        let entry2 = create_test_entry(name_id, file_id);

        let expected1 = staging.add_node(entry1);
        let expected2 = staging.add_node(entry2);

        let mapping = staging.commit_nodes(&mut arena).unwrap();

        assert_eq!(mapping.len(), 2);
        assert!(mapping.contains_key(&expected1));
        assert!(mapping.contains_key(&expected2));

        // Verify nodes were actually added
        let actual1 = mapping[&expected1];
        let actual2 = mapping[&expected2];
        assert!(arena.get(actual1).is_some());
        assert!(arena.get(actual2).is_some());
    }

    #[test]
    fn test_staging_remap_edges() {
        let mut staging = StagingGraph::new();
        let mut arena = NodeArena::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        // Add two nodes
        let entry1 = create_test_entry(name_id, file_id);
        let entry2 = create_test_entry(name_id, file_id);
        let expected1 = staging.add_node(entry1);
        let expected2 = staging.add_node(entry2);

        // Add edge using expected IDs
        staging.add_edge(
            expected1,
            expected2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        // Commit nodes
        let mapping = staging.commit_nodes(&mut arena).unwrap();

        // Get remapped edges
        let edges = staging.get_remapped_edges(&mapping);

        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, mapping[&expected1]);
        assert_eq!(edges[0].target, mapping[&expected2]);
    }

    #[test]
    fn test_staging_rollback() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        staging.add_node(create_test_entry(name_id, file_id));
        staging.add_edge(
            NodeId::new(0, 1),
            NodeId::new(1, 1),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        assert_eq!(staging.len(), 2);

        staging.rollback();

        assert!(staging.is_empty());
        assert_eq!(staging.stats().nodes_staged, 0);
        assert_eq!(staging.stats().edges_staged, 0);
    }

    #[test]
    fn test_staging_with_capacity() {
        let staging = StagingGraph::with_capacity(100, 200);
        assert!(staging.is_empty());
    }

    #[test]
    fn test_staging_add_edges_batch() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        let edges = vec![
            PendingEdge {
                source: NodeId::new(0, 1),
                target: NodeId::new(1, 1),
                kind: EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                file: file_id,
                spans: vec![],
            },
            PendingEdge {
                source: NodeId::new(1, 1),
                target: NodeId::new(2, 1),
                kind: EdgeKind::References,
                file: file_id,
                spans: vec![],
            },
        ];

        staging.add_edges(&edges);

        assert_eq!(staging.stats().edges_staged, 2);
    }

    #[test]
    fn test_staging_intern_string() {
        let mut staging = StagingGraph::new();
        let mut strings = StringInterner::new();

        // Use local StringIds (simulating what GraphBuildHelper does)
        let local_id_0 = StringId::new_local(0);
        let local_id_1 = StringId::new_local(1);
        staging.intern_string(local_id_0, "hello".to_string());
        staging.intern_string(local_id_1, "world".to_string());

        assert_eq!(staging.stats().strings_interned, 2);

        let _remap = staging
            .commit_strings(&mut strings)
            .expect("commit_strings should succeed");

        // Strings should now be interned
        let id1 = strings.intern("hello");
        let id2 = strings.intern("world");
        // Re-interning should return same IDs
        assert_eq!(strings.intern("hello"), id1);
        assert_eq!(strings.intern("world"), id2);
    }

    // ==================== StringId Lifecycle Tests ====================

    #[test]
    fn test_commit_strings_returns_remap_table() {
        let mut staging = StagingGraph::new();
        let mut strings = StringInterner::new();

        // Stage strings with local IDs
        let local_0 = StringId::new_local(0);
        let local_1 = StringId::new_local(1);
        let local_2 = StringId::new_local(2);

        staging.intern_string(local_0, "foo".to_string());
        staging.intern_string(local_1, "bar".to_string());
        staging.intern_string(local_2, "baz".to_string());

        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");

        // Remap table should contain all three local IDs
        assert_eq!(remap.len(), 3);
        assert!(remap.contains_key(&local_0));
        assert!(remap.contains_key(&local_1));
        assert!(remap.contains_key(&local_2));

        // Global IDs should resolve to correct strings
        let global_0 = remap[&local_0];
        let global_1 = remap[&local_1];
        let global_2 = remap[&local_2];

        assert_eq!(strings.resolve(global_0).unwrap().as_ref(), "foo");
        assert_eq!(strings.resolve(global_1).unwrap().as_ref(), "bar");
        assert_eq!(strings.resolve(global_2).unwrap().as_ref(), "baz");
    }

    #[test]
    fn test_apply_string_remap_updates_node_entry_name() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Create node with local StringId for name
        let local_name_id = StringId::new_local(0);
        staging.intern_string(local_name_id, "my_function".to_string());

        let entry = NodeEntry::new(NodeKind::Function, local_name_id, file_id);
        staging.add_node(entry);

        // Commit strings and get remap
        let mut strings = StringInterner::new();
        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");

        // Apply remap
        staging
            .apply_string_remap(&remap)
            .expect("apply_string_remap should succeed");

        // Verify node's name was updated
        let ops = staging.operations();
        if let StagingOp::AddNode { entry, .. } = &ops[1] {
            // ops[0] is InternString, ops[1] is AddNode
            let global_id = remap[&local_name_id];
            assert_eq!(entry.name, global_id);
        } else {
            panic!("Expected AddNode operation");
        }
    }

    #[test]
    fn test_apply_string_remap_updates_node_entry_optional_fields() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Create StringIds for all optional fields
        let local_name = StringId::new_local(0);
        let local_sig = StringId::new_local(1);
        let local_doc = StringId::new_local(2);
        let local_qn = StringId::new_local(3);
        let local_vis = StringId::new_local(4);

        staging.intern_string(local_name, "func".to_string());
        staging.intern_string(local_sig, "fn() -> i32".to_string());
        staging.intern_string(local_doc, "A function".to_string());
        staging.intern_string(local_qn, "module::func".to_string());
        staging.intern_string(local_vis, "pub".to_string());

        // Create node with all optional fields set
        let entry = NodeEntry::new(NodeKind::Function, local_name, file_id)
            .with_signature(local_sig)
            .with_doc(local_doc)
            .with_qualified_name(local_qn)
            .with_visibility(local_vis);
        staging.add_node(entry);

        // Commit and remap
        let mut strings = StringInterner::new();
        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");
        staging
            .apply_string_remap(&remap)
            .expect("apply_string_remap should succeed");

        // Verify all fields were remapped
        let ops = staging.operations();
        // Find the AddNode operation
        let node_entry = ops.iter().find_map(|op| {
            if let StagingOp::AddNode { entry, .. } = op {
                Some(entry)
            } else {
                None
            }
        });
        let entry = node_entry.expect("Should have AddNode op");

        assert_eq!(entry.name, remap[&local_name]);
        assert_eq!(entry.signature, Some(remap[&local_sig]));
        assert_eq!(entry.doc, Some(remap[&local_doc]));
        assert_eq!(entry.qualified_name, Some(remap[&local_qn]));
        assert_eq!(entry.visibility, Some(remap[&local_vis]));
    }

    #[test]
    fn test_apply_string_remap_updates_imports_alias() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Create local StringId for alias
        let local_alias = StringId::new_local(0);
        staging.intern_string(local_alias, "myAlias".to_string());

        // Add import edge with alias
        staging.add_edge(
            NodeId::new(0, 1),
            NodeId::new(1, 1),
            EdgeKind::Imports {
                alias: Some(local_alias),
                is_wildcard: false,
            },
            file_id,
        );

        // Commit and remap
        let mut strings = StringInterner::new();
        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");
        staging
            .apply_string_remap(&remap)
            .expect("apply_string_remap should succeed");

        // Verify alias was remapped
        let ops = staging.operations();
        let edge = ops.iter().find_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind)
            } else {
                None
            }
        });
        if let Some(EdgeKind::Imports { alias, .. }) = edge {
            assert_eq!(*alias, Some(remap[&local_alias]));
        } else {
            panic!("Expected Imports edge");
        }
    }

    #[test]
    fn test_apply_string_remap_updates_exports_alias() {
        use crate::graph::unified::edge::ExportKind;

        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Create local StringId for alias
        let local_alias = StringId::new_local(0);
        staging.intern_string(local_alias, "exportAlias".to_string());

        // Add export edge with alias
        staging.add_edge(
            NodeId::new(0, 1),
            NodeId::new(1, 1),
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: Some(local_alias),
            },
            file_id,
        );

        // Commit and remap
        let mut strings = StringInterner::new();
        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");
        staging
            .apply_string_remap(&remap)
            .expect("apply_string_remap should succeed");

        // Verify alias was remapped
        let ops = staging.operations();
        let edge = ops.iter().find_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind)
            } else {
                None
            }
        });
        if let Some(EdgeKind::Exports { alias, .. }) = edge {
            assert_eq!(*alias, Some(remap[&local_alias]));
        } else {
            panic!("Expected Exports edge");
        }
    }

    #[test]
    fn test_apply_string_remap_updates_grpc_call() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Create local StringIds for service and method
        let local_service = StringId::new_local(0);
        let local_method = StringId::new_local(1);
        staging.intern_string(local_service, "UserService".to_string());
        staging.intern_string(local_method, "GetUser".to_string());

        // Add GrpcCall edge
        staging.add_edge(
            NodeId::new(0, 1),
            NodeId::new(1, 1),
            EdgeKind::GrpcCall {
                service: local_service,
                method: local_method,
            },
            file_id,
        );

        // Commit and remap
        let mut strings = StringInterner::new();
        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");
        staging
            .apply_string_remap(&remap)
            .expect("apply_string_remap should succeed");

        // Verify service and method were remapped
        let ops = staging.operations();
        let edge = ops.iter().find_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind)
            } else {
                None
            }
        });
        if let Some(EdgeKind::GrpcCall { service, method }) = edge {
            assert_eq!(*service, remap[&local_service]);
            assert_eq!(*method, remap[&local_method]);
        } else {
            panic!("Expected GrpcCall edge");
        }
    }

    #[test]
    fn test_apply_string_remap_updates_mq_protocol_other() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Create local StringIds for protocol and topic
        let local_protocol = StringId::new_local(0);
        let local_topic = StringId::new_local(1);
        staging.intern_string(local_protocol, "custom_mq".to_string());
        staging.intern_string(local_topic, "events".to_string());

        // Add MessageQueue edge with Other protocol
        staging.add_edge(
            NodeId::new(0, 1),
            NodeId::new(1, 1),
            EdgeKind::MessageQueue {
                protocol: MqProtocol::Other(local_protocol),
                topic: Some(local_topic),
            },
            file_id,
        );

        // Commit and remap
        let mut strings = StringInterner::new();
        let remap = staging
            .commit_strings(&mut strings)
            .expect("commit should succeed");
        staging
            .apply_string_remap(&remap)
            .expect("apply_string_remap should succeed");

        // Verify protocol and topic were remapped
        let ops = staging.operations();
        let edge = ops.iter().find_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind)
            } else {
                None
            }
        });
        if let Some(EdgeKind::MessageQueue { protocol, topic }) = edge {
            if let MqProtocol::Other(protocol_id) = protocol {
                assert_eq!(*protocol_id, remap[&local_protocol]);
            } else {
                panic!("Expected MqProtocol::Other");
            }
            assert_eq!(*topic, Some(remap[&local_topic]));
        } else {
            panic!("Expected MessageQueue edge");
        }
    }

    #[test]
    fn test_duplicate_local_string_id_error() {
        let mut staging = StagingGraph::new();

        // Stage same local_id twice (this should never happen in normal usage)
        let local_id = StringId::new_local(0);
        staging.intern_string(local_id, "first".to_string());
        staging.intern_string(local_id, "second".to_string()); // Duplicate!

        let mut strings = StringInterner::new();
        let result = staging.commit_strings(&mut strings);

        assert!(result.is_err());
        if let Err(StagingError::DuplicateLocalStringId { local_id: err_id }) = result {
            assert_eq!(err_id, local_id);
        } else {
            panic!("Expected DuplicateLocalStringId error");
        }
    }

    #[test]
    fn test_commit_strings_rejects_non_local_intern_string_id() {
        let mut staging = StagingGraph::new();
        let mut strings = StringInterner::new();

        // Non-local StringId (should never be used for InternString.local_id)
        staging.intern_string(StringId::new(1), "oops".to_string());

        let result = staging.commit_strings(&mut strings);
        assert!(matches!(
            result,
            Err(StagingError::NonLocalInternStringId { .. })
        ));
    }

    #[test]
    fn test_apply_string_remap_errors_on_unmapped_local_string_id() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Local StringId used in a NodeEntry, but never staged via InternString.
        let local_name_id = StringId::new_local(0);
        staging.add_node(NodeEntry::new(NodeKind::Function, local_name_id, file_id));

        let remap = std::collections::HashMap::new();
        let result = staging.apply_string_remap(&remap);
        assert!(matches!(
            result,
            Err(StagingError::UnmappedLocalStringId { .. })
        ));
    }

    #[test]
    fn test_intern_capacity_exhausted_error() {
        let mut staging = StagingGraph::new();

        // Stage more strings than the interner can hold
        for i in 0..10 {
            staging.intern_string(StringId::new_local(i), format!("string_{i}"));
        }

        // Use an interner with a small max_ids limit
        let mut strings = StringInterner::with_max_ids(5);
        let result = staging.commit_strings(&mut strings);

        assert!(result.is_err());
        assert!(matches!(result, Err(StagingError::InternCapacityExhausted)));
    }

    #[test]
    fn test_full_commit_flow_with_string_remap() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        // Stage strings
        let local_func_name = StringId::new_local(0);
        let local_callee_name = StringId::new_local(1);
        staging.intern_string(local_func_name, "main".to_string());
        staging.intern_string(local_callee_name, "helper".to_string());

        // Stage nodes with local StringIds
        let func_entry = NodeEntry::new(NodeKind::Function, local_func_name, file_id);
        let callee_entry = NodeEntry::new(NodeKind::Function, local_callee_name, file_id);
        let func_id = staging.add_node(func_entry);
        let callee_id = staging.add_node(callee_entry);

        // Stage edge
        staging.add_edge(
            func_id,
            callee_id,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        // Full commit flow
        let mut strings = StringInterner::new();
        let mut arena = NodeArena::new();

        // 1. Commit strings
        let string_remap = staging
            .commit_strings(&mut strings)
            .expect("commit_strings should succeed");

        // 2. Apply string remap
        staging
            .apply_string_remap(&string_remap)
            .expect("apply_string_remap should succeed");

        // 3. Commit nodes
        let node_remap = staging
            .commit_nodes(&mut arena)
            .expect("commit_nodes should succeed");

        // 4. Get remapped edges
        let edges = staging.get_remapped_edges(&node_remap);

        // Verify the result
        assert_eq!(arena.len(), 2);
        assert_eq!(strings.len(), 2);
        assert_eq!(edges.len(), 1);

        // Verify node names resolve to correct strings
        let actual_func = arena.get(node_remap[&func_id]).unwrap();
        let actual_callee = arena.get(node_remap[&callee_id]).unwrap();

        assert_eq!(strings.resolve(actual_func.name).unwrap().as_ref(), "main");
        assert_eq!(
            strings.resolve(actual_callee.name).unwrap().as_ref(),
            "helper"
        );
    }

    #[test]
    fn test_apply_file_id_updates_staged_operations() {
        let mut staging = StagingGraph::new();
        let local_file_id = FileId::new(0);
        let global_file_id = FileId::new(1);
        let name_id = StringId::new(0);

        let node_a = staging.add_node(NodeEntry::new(NodeKind::Function, name_id, local_file_id));
        let node_b = staging.add_node(NodeEntry::new(NodeKind::Function, name_id, local_file_id));
        staging.add_edge(node_a, node_b, EdgeKind::References, local_file_id);

        staging.apply_file_id(global_file_id);

        let mut seen_node = false;
        let mut seen_edge = false;
        for op in staging.operations() {
            match op {
                StagingOp::AddNode { entry, .. } => {
                    assert_eq!(entry.file, global_file_id);
                    seen_node = true;
                }
                StagingOp::AddEdge { file, .. } => {
                    assert_eq!(*file, global_file_id);
                    seen_edge = true;
                }
                _ => {}
            }
        }
        assert!(seen_node);
        assert!(seen_edge);
    }

    #[test]
    fn test_confidence_metadata_storage() {
        let mut staging = StagingGraph::new();

        // Initially no confidence
        assert!(staging.confidence().is_none());

        // Set confidence
        let confidence = ConfidenceMetadata {
            level: crate::confidence::ConfidenceLevel::Partial,
            limitations: vec!["Test limitation".to_string()],
            unavailable_features: vec!["test_feature".to_string()],
        };
        staging.set_confidence(confidence.clone());

        // Verify it was stored
        let stored = staging.confidence().expect("confidence should be set");
        assert_eq!(stored.level, crate::confidence::ConfidenceLevel::Partial);
        assert_eq!(stored.limitations, vec!["Test limitation"]);
        assert_eq!(stored.unavailable_features, vec!["test_feature"]);
    }

    #[test]
    fn test_confidence_metadata_take() {
        let mut staging = StagingGraph::new();

        let confidence = ConfidenceMetadata {
            level: crate::confidence::ConfidenceLevel::AstOnly,
            limitations: vec![],
            unavailable_features: vec![],
        };
        staging.set_confidence(confidence);

        // Take the confidence
        let taken = staging.take_confidence().expect("confidence should exist");
        assert_eq!(taken.level, crate::confidence::ConfidenceLevel::AstOnly);

        // Should be None after taking
        assert!(staging.confidence().is_none());
    }

    #[test]
    fn test_confidence_cleared_on_rollback() {
        let mut staging = StagingGraph::new();

        let confidence = ConfidenceMetadata {
            level: crate::confidence::ConfidenceLevel::Verified,
            limitations: vec![],
            unavailable_features: vec![],
        };
        staging.set_confidence(confidence);

        // Rollback should clear confidence
        staging.rollback();
        assert!(staging.confidence().is_none());
    }

    #[test]
    fn test_attach_body_hashes() {
        use crate::graph::unified::build::body_hash::node_kind_supports_body_hash;

        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        // Create a function entry with valid location
        let mut func_entry = NodeEntry::new(NodeKind::Function, name_id, file_id);
        func_entry.start_line = 1;
        func_entry.start_column = 0;
        func_entry.end_line = 1;
        func_entry.end_column = 23; // "fn foo() { return 42; }"

        // Create a variable entry (non-hashable kind)
        let mut var_entry = NodeEntry::new(NodeKind::Variable, name_id, file_id);
        var_entry.start_line = 2;
        var_entry.start_column = 0;
        var_entry.end_line = 2;
        var_entry.end_column = 10;

        staging.add_node(func_entry);
        staging.add_node(var_entry);

        // Content for the file
        let content = b"fn foo() { return 42; }\nlet x = 42";

        // Before attach, both should have None body_hash
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, .. } = op {
                assert!(entry.body_hash.is_none());
            }
        }

        // Attach body hashes (no shape context: this test exercises body_hash only)
        staging.attach_body_hashes(content, None);

        // After attach, only the function should have body_hash set
        let mut func_has_hash = false;
        let mut var_has_hash = false;
        for op in staging.operations() {
            if let StagingOp::AddNode { entry, .. } = op {
                match entry.kind {
                    NodeKind::Function => {
                        assert!(entry.body_hash.is_some());
                        func_has_hash = true;
                    }
                    NodeKind::Variable => {
                        assert!(entry.body_hash.is_none()); // Variables don't support hashing
                        assert!(!node_kind_supports_body_hash(NodeKind::Variable));
                        var_has_hash = entry.body_hash.is_some();
                    }
                    _ => {}
                }
            }
        }

        assert!(func_has_hash, "Function should have body hash");
        assert!(!var_has_hash, "Variable should NOT have body hash");
    }

    /// Minimal in-crate [`ShapeMapping`] so the staging seam can be exercised
    /// without a language-plugin dependency (sqry-core cannot depend on
    /// sqry-lang-rust). Covers just enough Rust kinds to drive the walker.
    struct SeamTestMapping;

    impl crate::graph::unified::build::shape::ShapeMapping for SeamTestMapping {
        fn cf_bucket(&self, id: u16) -> Option<crate::graph::unified::build::shape::CfBucket> {
            use crate::graph::unified::build::shape::CfBucket;
            let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
            match lang.node_kind_for_id(id)? {
                "if_expression" => Some(CfBucket::Branch),
                "return_expression" => Some(CfBucket::Return),
                "let_declaration" => Some(CfBucket::Assign),
                "call_expression" => Some(CfBucket::Call),
                _ => None,
            }
        }

        fn signature_shape(
            &self,
            fn_node: tree_sitter::Node,
            _src: &[u8],
        ) -> crate::graph::unified::storage::shape::SignatureShape {
            let mut s = crate::graph::unified::storage::shape::SignatureShape::default();
            if let Some(p) = fn_node.child_by_field_name("parameters") {
                let mut c = p.walk();
                for child in p.named_children(&mut c) {
                    if matches!(child.kind(), "parameter" | "self_parameter") {
                        s.arity_positional = s.arity_positional.saturating_add(1);
                    }
                }
            }
            s
        }
    }

    fn parse_rust(src: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&lang).expect("load rust grammar");
        parser.parse(src, None).expect("parse")
    }

    fn first_function_item(tree: &tree_sitter::Tree) -> tree_sitter::Node<'_> {
        let root = tree.root_node();
        let mut cursor = root.walk();
        root.named_children(&mut cursor)
            .find(|n| n.kind() == "function_item")
            .expect("function_item present")
    }

    #[test]
    fn test_attach_shape_descriptors_targets_staging_store_for_function_only() {
        use crate::graph::unified::build::shape::CfBucket;

        let src = "fn foo(x: i32) -> i32 { let y = x + 1; if y > 0 { return y; } y }";
        let tree = parse_rust(src);
        let func = first_function_item(&tree);
        let sp = func.start_position();
        let ep = func.end_position();

        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        // Function node spanning the whole function_item (1-indexed lines, mirroring
        // the build helper's span->location conversion).
        let mut func_entry = NodeEntry::new(NodeKind::Function, name_id, file_id);
        func_entry.start_line = u32::try_from(sp.row).unwrap() + 1;
        func_entry.start_column = u32::try_from(sp.column).unwrap();
        func_entry.end_line = u32::try_from(ep.row).unwrap() + 1;
        func_entry.end_column = u32::try_from(ep.column).unwrap();
        let func_node_id = staging.add_node(func_entry);

        // A non-Function kind with a valid span: must be skipped by the kind gate.
        let mut var_entry = NodeEntry::new(NodeKind::Variable, name_id, file_id);
        var_entry.start_line = 1;
        var_entry.start_column = 24;
        var_entry.end_line = 1;
        var_entry.end_column = 38;
        let var_node_id = staging.add_node(var_entry);

        let ctx = ShapeAttachCtx::new(&tree, src.as_bytes(), &SeamTestMapping);
        staging.attach_body_hashes(src.as_bytes(), Some(&ctx));

        let metadata = staging.take_macro_metadata();
        let descriptors = metadata.shape_descriptors();
        assert!(
            descriptors.contains_key(&func_node_id),
            "descriptor must be keyed by the staging-local NodeId in the take_macro_metadata store"
        );
        assert!(
            !descriptors.contains_key(&var_node_id),
            "non-Function/Method kinds get no descriptor"
        );
        let d = &descriptors[&func_node_id];
        assert_eq!(d.cf_histogram[CfBucket::Branch.index()], 1);
        assert_eq!(d.cf_histogram[CfBucket::Return.index()], 1);
        assert_eq!(d.cf_histogram[CfBucket::Assign.index()], 1);
        assert_eq!(d.signature_shape.arity_positional, 1);
        assert!(!d.is_unhashable());
    }

    #[test]
    fn test_attach_body_hashes_none_ctx_produces_no_descriptors() {
        let src = "fn foo(x: i32) -> i32 { let y = x + 1; if y > 0 { return y; } y }";
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);
        let mut func_entry = NodeEntry::new(NodeKind::Function, name_id, file_id);
        func_entry.start_line = 1;
        func_entry.start_column = 0;
        func_entry.end_line = 1;
        func_entry.end_column = u32::try_from(src.len()).unwrap();
        staging.add_node(func_entry);

        // No shape context: body hashes only, the pre-feature behaviour.
        staging.attach_body_hashes(src.as_bytes(), None);

        assert!(
            staging.take_macro_metadata().shape_descriptors().is_empty(),
            "without a shape context no descriptors are produced"
        );
    }

    // ==================== Query API Tests ====================

    #[test]
    fn test_nodes_iterator() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        let id1 = staging.add_node(NodeEntry::new(
            NodeKind::Function,
            StringId::new(1),
            file_id,
        ));
        let id2 = staging.add_node(NodeEntry::new(NodeKind::Class, StringId::new(2), file_id));
        let id3 = staging.add_node(NodeEntry::new(NodeKind::Module, StringId::new(3), file_id));

        // Also add an edge to make sure it's filtered out
        staging.add_edge(
            id1,
            id2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );

        let nodes: Vec<_> = staging.nodes().collect();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].entry.kind, NodeKind::Function);
        assert_eq!(nodes[0].expected_id, Some(id1));
        assert_eq!(nodes[1].entry.kind, NodeKind::Class);
        assert_eq!(nodes[1].expected_id, Some(id2));
        assert_eq!(nodes[2].entry.kind, NodeKind::Module);
        assert_eq!(nodes[2].expected_id, Some(id3));
    }

    #[test]
    fn test_edges_iterator() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        let id1 = staging.add_node(NodeEntry::new(
            NodeKind::Function,
            StringId::new(1),
            file_id,
        ));
        let id2 = staging.add_node(NodeEntry::new(
            NodeKind::Function,
            StringId::new(2),
            file_id,
        ));

        staging.add_edge(
            id1,
            id2,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        staging.add_edge(id1, id2, EdgeKind::References, file_id);

        let edges: Vec<_> = staging.edges().collect();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].source, id1);
        assert_eq!(edges[0].target, id2);
        assert!(matches!(
            edges[0].kind,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            }
        ));
        assert_eq!(edges[0].file, file_id);
        assert!(matches!(edges[1].kind, EdgeKind::References));
    }

    #[test]
    fn test_node_count_and_edge_count() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        assert_eq!(staging.node_count(), 0);
        assert_eq!(staging.edge_count(), 0);

        let id1 = staging.add_node(NodeEntry::new(
            NodeKind::Function,
            StringId::new(1),
            file_id,
        ));
        let id2 = staging.add_node(NodeEntry::new(
            NodeKind::Function,
            StringId::new(2),
            file_id,
        ));
        assert_eq!(staging.node_count(), 2);
        assert_eq!(staging.edge_count(), 0);

        staging.add_edge(
            id1,
            id2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        assert_eq!(staging.node_count(), 2);
        assert_eq!(staging.edge_count(), 1);
    }

    #[test]
    fn test_resolve_local_string() {
        let mut staging = StagingGraph::new();

        let local_0 = StringId::new_local(0);
        let local_1 = StringId::new_local(1);
        staging.intern_string(local_0, "hello".to_string());
        staging.intern_string(local_1, "world".to_string());

        assert_eq!(staging.resolve_local_string(local_0), Some("hello"));
        assert_eq!(staging.resolve_local_string(local_1), Some("world"));
        assert_eq!(staging.resolve_local_string(StringId::new_local(99)), None);
    }

    #[test]
    fn test_resolve_node_name_prefers_qualified() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        let name_id = StringId::new_local(0);
        let qname_id = StringId::new_local(1);
        staging.intern_string(name_id, "foo".to_string());
        staging.intern_string(qname_id, "bar::foo".to_string());

        // Node without qualified_name -> falls back to name
        let entry_no_qn = NodeEntry::new(NodeKind::Function, name_id, file_id);
        assert_eq!(
            staging.resolve_node_canonical_name(&entry_no_qn),
            Some("foo")
        );
        assert_eq!(staging.resolve_node_name(&entry_no_qn), Some("foo"));

        // Node with qualified_name -> prefers qualified
        let entry_with_qn =
            NodeEntry::new(NodeKind::Function, name_id, file_id).with_qualified_name(qname_id);
        assert_eq!(
            staging.resolve_node_canonical_name(&entry_with_qn),
            Some("bar::foo")
        );
        assert_eq!(staging.resolve_node_name(&entry_with_qn), Some("bar::foo"));
    }

    #[test]
    fn test_resolve_node_display_name_uses_native_language_separator() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        let name_id = StringId::new_local(0);
        let qname_id = StringId::new_local(1);
        staging.intern_string(name_id, "count".to_string());
        staging.intern_string(qname_id, "Counter::count".to_string());

        let entry =
            NodeEntry::new(NodeKind::Property, name_id, file_id).with_qualified_name(qname_id);
        assert_eq!(
            staging.resolve_node_display_name(Language::Dart, &entry),
            Some("Counter.count".to_string())
        );
        assert_eq!(
            staging.resolve_node_canonical_name(&entry),
            Some("Counter::count")
        );
    }

    #[test]
    fn test_resolve_node_display_name_respects_ruby_method_convention() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        let name_id = StringId::new_local(0);
        let qname_id = StringId::new_local(1);
        staging.intern_string(name_id, "authenticate".to_string());
        staging.intern_string(qname_id, "User::authenticate".to_string());

        let entry =
            NodeEntry::new(NodeKind::Method, name_id, file_id).with_qualified_name(qname_id);
        assert_eq!(
            staging.resolve_node_display_name(Language::Ruby, &entry),
            Some("User#authenticate".to_string())
        );
    }

    #[test]
    fn test_nodes_empty_after_rollback() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);

        staging.add_node(NodeEntry::new(
            NodeKind::Function,
            StringId::new(1),
            file_id,
        ));
        staging.add_edge(
            NodeId::new(0, 1),
            NodeId::new(1, 1),
            EdgeKind::References,
            file_id,
        );

        assert_eq!(staging.node_count(), 1);
        assert_eq!(staging.edge_count(), 1);
        assert_eq!(staging.nodes().count(), 1);
        assert_eq!(staging.edges().count(), 1);

        staging.rollback();

        assert_eq!(staging.node_count(), 0);
        assert_eq!(staging.edge_count(), 0);
        assert_eq!(staging.nodes().count(), 0);
        assert_eq!(staging.edges().count(), 0);
    }

    #[test]
    fn test_estimated_byte_size_empty() {
        let staging = StagingGraph::new();
        assert_eq!(staging.estimated_byte_size(), 0);
    }

    #[test]
    fn test_estimated_byte_size_with_ops() {
        let mut staging = StagingGraph::new();
        let entry = NodeEntry::new(NodeKind::Function, StringId::INVALID, FileId::new(0));
        staging.add_node(entry);
        assert!(staging.estimated_byte_size() > 0);
    }

    #[test]
    fn test_estimated_byte_size_includes_heap_allocations() {
        let mut staging = StagingGraph::new();
        let base_size = staging.estimated_byte_size();
        assert_eq!(base_size, 0);

        // InternString has a heap-allocated String value.
        let long_string = "a".repeat(1024);
        let local_id = StringId::new_local(0);
        staging.intern_string(local_id, long_string.clone());
        let after_intern = staging.estimated_byte_size();

        // The size must include the heap bytes from the string (>= 1024).
        let op_overhead = std::mem::size_of::<StagingOp>();
        assert!(
            after_intern >= op_overhead + 1024,
            "estimated_byte_size should account for heap: got {after_intern}, \
             expected at least {} (op={op_overhead} + string=1024)",
            op_overhead + 1024,
        );

        // RegisterFile also has a heap-allocated path String.
        staging.register_file("/some/long/path/to/a/file.rs".to_string(), FileId::new(0));
        let after_file = staging.estimated_byte_size();
        assert!(
            after_file > after_intern,
            "RegisterFile path should add to estimated_byte_size"
        );
    }

    #[test]
    fn test_staging_count_accessors_initial() {
        let staging = StagingGraph::new();
        assert_eq!(staging.node_count_u32(), 0);
        assert_eq!(staging.string_count_u32(), 0);
        assert_eq!(staging.edge_count_u32(), 0);
    }

    #[test]
    fn test_staging_count_accessors_after_staging() {
        let mut staging = StagingGraph::new();
        let file_id = FileId::new(0);
        let name_id = StringId::new(1);

        // Stage two nodes
        let entry1 = create_test_entry(name_id, file_id);
        let node1 = staging.add_node(entry1);
        let entry2 = create_test_entry(name_id, file_id);
        let node2 = staging.add_node(entry2);
        assert_eq!(staging.node_count_u32(), 2);

        // Stage an edge
        staging.add_edge(
            node1,
            node2,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
        assert_eq!(staging.edge_count_u32(), 1);

        // Intern a string
        staging.intern_string(StringId::new(10), "hello".to_string());
        assert_eq!(staging.string_count_u32(), 1);

        // Verify all together
        assert_eq!(staging.node_count_u32(), 2);
        assert_eq!(staging.string_count_u32(), 1);
        assert_eq!(staging.edge_count_u32(), 1);
    }
}
