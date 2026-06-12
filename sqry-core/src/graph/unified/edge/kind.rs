//! `EdgeKind` enumeration for the unified graph architecture.
//!
//! This module defines `EdgeKind`, which categorizes all relationship types
//! that can be represented as edges in the graph.
//!
//! # Design (, Appendix A2)
//!
//! The enumeration covers:
//! - **Structural**: Defines, Contains
//! - **References**: Calls, References, Imports, Exports, `TypeOf`
//! - **OOP**: Inherits, Implements
//! - **Cross-language**: FFI, HTTP, gRPC, WebAssembly, DB queries
//! - **Extended**: `MessageQueue`, WebSocket, GraphQL, `ProcessExec`, `FileIpc`

use std::fmt;

use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::super::string::StringId;

/// Resolution provenance for a `Calls` edge.
///
/// Discriminates how the call target was resolved during graph construction.
/// Introduced by C-icall-precision Phase A (DESIGN §6); extended in Phase β
/// joint-stubs (V12) with 5 additional dispatch-resolver provenances Plan B
/// WS2 populates (`graph-fidelity-planner-correctness-dag.toml` line 221-239,
/// DESIGN §3.2).
///
/// # Semantics
///
/// - `Direct` — the call target was resolved by the language plugin from a
///   syntactic call expression (e.g., `f(x)` where `f` resolves to a single
///   definition). This is the default and applies to every pre-Phase-A
///   `Calls` edge (V10 wire compatibility).
/// - `TypeMatch` — the call target was resolved post-hoc by flat type matching
///   of indirect-call sites against compatible signatures. (Plan B DESIGN
///   §3.2 names this `IndirectTypeMatch`; the V11 wire form names it
///   `TypeMatch` and we keep that name for postcard on-disk stability.)
/// - `BindingPlane` — the call target was resolved via the binding-plane
///   designated-initializer mechanism (struct-field-of-function-pointer
///   construction site witnesses). (Plan B DESIGN §3.2 names this
///   `IndirectBindingPlane`; same naming-stability rationale as above.)
/// - `VirtualDispatch` — JVM virtual / abstract method dispatch resolved
///   through `Implements`/`Inherits` walks (Plan B `pass5c_jvm_virtual`).
/// - `InterfaceDispatch` — Go interface dispatch resolved via structural
///   method-set superset (Plan B `pass5d_go_interface`).
/// - `DuckTyped` — Python duck-typed dispatch resolved by name+arity match
///   on unknown-receiver call sites (Plan B `pass5e_python_duck`).
/// - `Structural` — TypeScript structural dispatch resolved by declared
///   interface superset (Plan B `pass5f_ts_structural`).
/// - `PromiscuousElided` — fan-out cap exceeded (`CALLSITE_PROMISCUOUS`);
///   resolver emitted a diagnostic self-edge instead of N targets.
///
/// # Wire compatibility (V11 → V12)
///
/// `ResolvedVia` is `#[repr(u16)]` with **explicit pinned discriminants**
/// (0..=7) for V12 on-disk stability. Re-ordering or re-assigning these
/// values is a snapshot-format breaking change — see Plan B DAG
/// `critical_decisions` line 233 and DESIGN §3.2 line 239 ("Discriminants
/// pinned: changing them later breaks V12 snapshots").
///
/// The serde `rename_all = "snake_case"` attribute governs JSON / human
/// wire forms (planner text frontend, MCP filter params): the names emit
/// as `direct`, `type_match`, `binding_plane`, `virtual_dispatch`,
/// `interface_dispatch`, `duck_typed`, `structural`, `promiscuous_elided`.
///
/// Pre-Phase-A `Calls` payloads in **JSON** that omit the field
/// deserialize with `ResolvedVia::Direct` (via `#[serde(default)]` on the
/// `EdgeKind::Calls.resolved_via` field) — see test
/// `calls_edge_json_default_old_wire` below.
///
/// **Postcard (the on-disk snapshot format) is positional and does NOT
/// have a field-absence concept**, so `#[serde(default)]` cannot rescue a
/// V10-shape postcard `Calls` payload (3 bytes: `[variant, argument_count,
/// is_async]`). V10 → V11 postcard forward-compat is implemented in
/// `sqry-core/src/graph/unified/persistence/snapshot.rs::upconvert_v10_to_v11`
/// via explicit V10 type translation — that is the canonical V10 postcard
/// reader path, not this serde annotation. V11 → V12 inherits this discipline:
/// pre-V12 `ResolvedVia` payloads only carry variants 0..=2, and the V11
/// upconvert preserves them unchanged.
///
/// # Why not an `EdgeKind::FfiCall` member
///
/// FFI calls are a distinct `EdgeKind` variant (`EdgeKind::FfiCall`) with
/// their own metadata. `ResolvedVia` discriminates resolution strategy within
/// the `Calls` variant, not edge-kind identity.
#[repr(u16)]
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedVia {
    /// Resolved directly by the language plugin from a syntactic call.
    /// Pinned discriminant `0` for V12 on-disk stability.
    #[default]
    Direct = 0,
    /// Resolved by flat type matching of an indirect call against compatible signatures.
    /// Pinned discriminant `1` for V12 on-disk stability.
    TypeMatch = 1,
    /// Resolved via binding-plane designated-initializer witnesses.
    /// Pinned discriminant `2` for V12 on-disk stability.
    BindingPlane = 2,
    /// JVM virtual / abstract method dispatch (Plan B `pass5c_jvm_virtual`).
    /// Pinned discriminant `3` for V12 on-disk stability.
    VirtualDispatch = 3,
    /// Go interface dispatch (Plan B `pass5d_go_interface`).
    /// Pinned discriminant `4` for V12 on-disk stability.
    InterfaceDispatch = 4,
    /// Python duck-typed dispatch (Plan B `pass5e_python_duck`).
    /// Pinned discriminant `5` for V12 on-disk stability.
    DuckTyped = 5,
    /// TypeScript structural dispatch (Plan B `pass5f_ts_structural`).
    /// Pinned discriminant `6` for V12 on-disk stability.
    Structural = 6,
    /// `CALLSITE_PROMISCUOUS` fan-out cap exceeded — resolver emitted a
    /// diagnostic self-edge instead of N targets. Pinned discriminant `7`
    /// for V12 on-disk stability.
    PromiscuousElided = 7,
}

impl ResolvedVia {
    /// Returns the pinned `u16` discriminant. Stable across V12 releases.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    /// All variants in pinned discriminant order. Convenience for tests
    /// and downstream consumers that need to enumerate the resolution
    /// provenance set.
    pub const ALL: &'static [ResolvedVia] = &[
        ResolvedVia::Direct,
        ResolvedVia::TypeMatch,
        ResolvedVia::BindingPlane,
        ResolvedVia::VirtualDispatch,
        ResolvedVia::InterfaceDispatch,
        ResolvedVia::DuckTyped,
        ResolvedVia::Structural,
        ResolvedVia::PromiscuousElided,
    ];
}

/// Context for `TypeOf` edges (parameter, return, field, variable).
///
/// Indicates where a type reference appears in the code structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypeOfContext {
    /// Function/method parameter
    Parameter,
    /// Function/method return value
    Return,
    /// Struct/class field
    Field,
    /// Variable declaration
    Variable,
    /// Type parameter (generics)
    TypeParameter,
    /// Type constraint
    Constraint,
}

/// FFI calling convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FfiConvention {
    /// Standard C calling convention
    #[default]
    C,
    /// cdecl calling convention
    Cdecl,
    /// stdcall calling convention (Windows)
    Stdcall,
    /// fastcall calling convention
    Fastcall,
    /// System default calling convention
    System,
}

/// HTTP method for HTTP request edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[derive(Default)]
pub enum HttpMethod {
    /// GET request
    #[default]
    Get,
    /// POST request
    Post,
    /// PUT request
    Put,
    /// DELETE request
    Delete,
    /// PATCH request
    Patch,
    /// HEAD request
    Head,
    /// OPTIONS request
    Options,
    /// ALL methods (wildcard — matches any HTTP method)
    All,
}

impl HttpMethod {
    /// Returns the HTTP method as a string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
            Self::Patch => "PATCH",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
            Self::All => "ALL",
        }
    }
}

/// Database query type for DB query edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum DbQueryType {
    /// SELECT query
    #[default]
    Select,
    /// INSERT query
    Insert,
    /// UPDATE query
    Update,
    /// DELETE query
    Delete,
    /// EXECUTE stored procedure/function
    Execute,
}

/// Database table write operation (SQL).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableWriteOp {
    /// INSERT operation
    Insert,
    /// UPDATE operation
    Update,
    /// DELETE operation
    Delete,
}

/// Export kind for distinguishing re-exports from declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ExportKind {
    /// Direct export of a symbol
    #[default]
    Direct,
    /// Re-export from another module
    Reexport,
    /// Default export (JavaScript/TypeScript)
    Default,
    /// Namespace export (export *)
    Namespace,
}

/// Message queue protocol for async communication edges.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MqProtocol {
    /// Apache Kafka
    #[default]
    Kafka,
    /// AWS SQS
    Sqs,
    /// `RabbitMQ` / AMQP
    RabbitMq,
    /// NATS messaging
    Nats,
    /// Redis Pub/Sub
    Redis,
    /// Other protocol (identified by `StringId`)
    Other(StringId),
}

/// Kind of lifetime constraint relationship (Rust-specific).
///
/// Models the various ways lifetimes can constrain other lifetimes or types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum LifetimeConstraintKind {
    /// `'a: 'b` - lifetime 'a outlives 'b
    #[default]
    Outlives,
    /// `T: 'a` - type T is bounded by lifetime 'a
    TypeBound,
    /// `&'a T` - reference with explicit lifetime
    Reference,
    /// `'static` bound
    Static,
    /// Higher-ranked trait bound: `for<'a> T: Trait<'a>`
    HigherRanked,
    /// Trait object bound: `dyn Trait + 'a`
    TraitObject,
    /// impl Trait bound: `impl Trait + 'a`
    ImplTrait,
    /// Elided lifetime (inferred by compiler, requires RA)
    Elided,
}

/// Kind of macro expansion (Rust-specific).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum MacroExpansionKind {
    /// Derive macro (`#[derive(...)]`)
    Derive,
    /// Attribute macro (`#[proc_macro_attribute]`)
    Attribute,
    /// Declarative macro (`macro_rules!`)
    #[default]
    Declarative,
    /// Function-like macro
    Function,
    /// Conditional compilation gate (`#[cfg(...)]`, `#[cfg_attr(...)]`)
    CfgGate,
}

/// Kind of error-wrapping relationship (T3 — Go error chains).
///
/// Distinguishes the seven source-syntax forms that produce a
/// [`EdgeKind::Wraps`] edge. Each variant identifies the construct that
/// authored the edge so downstream queries can filter by origin (e.g.
/// "show me only `%w` format wraps" vs. "show me `Unwrap()` method wraps").
///
/// Variant ordering is significant for postcard serialization stability —
/// add new variants at the end (after `ErrorsJoin`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WrapKind {
    /// `fmt.Errorf("...%w...", err)` — `%w` format verb wrapping.
    #[default]
    ErrorfVerb,
    /// `func (e *E) Unwrap() error { return e.inner }` — single-error
    /// `Unwrap` method.
    UnwrapMethod,
    /// `func (e *E) Unwrap() []error { return e.errs }` — multi-error
    /// `Unwrap` method (Go 1.20+).
    UnwrapMultiMethod,
    /// `errors.Is(err, sentinel)` — sentinel-error comparison.
    ErrorsIs,
    /// `errors.As(err, &target)` — concrete-type extraction (target by reference).
    ErrorsAs,
    /// `errors.AsType[E](err)` — typed extraction (Go 1.26+).
    ErrorsAsType,
    /// `errors.Join(errs...)` — variadic joining (Go 1.20+).
    ErrorsJoin,
}

/// Direction of a channel operation (Go T2.4).
///
/// Discriminates whether a [`EdgeKind::ChannelPeer`] edge records a send,
/// a receive, or a close on the target [`super::super::node::kind::NodeKind::Channel`].
/// Aligns with `GoGuard`'s producer / consumer abstraction (see
/// `docs/development/go-channels-and-generic-instantiation/02_DESIGN.md` §1.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPeerDirection {
    /// `ch <- v` send.
    Send,
    /// `<-ch` receive (expression, short-var, range, select receive arm).
    Receive,
    /// `close(ch)` builtin call.
    Close,
}

/// Buffer classification of the channel an operation acts on (Go T2.4).
///
/// Cached on each [`EdgeKind::ChannelPeer`] edge from the owning `Channel`
/// node so the planner can filter without joining through the node. The
/// numeric capacity (for `Buffered`) lives on the `Channel` node metadata,
/// not on the edge, to keep edge payloads compact across millions of edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelBufferKind {
    /// `make(chan T)` — zero capacity.
    Unbuffered,
    /// `make(chan T, N)` with `N` resolved to a constant.
    Buffered,
    /// Capacity expression was non-constant, or the channel was reached
    /// through a parameter / struct-field where the alias resolver did not
    /// see the original `make` call.
    Unknown,
}

/// How a generic instantiation's type-argument vector was derived (Go T2.5).
///
/// Carried on each [`EdgeKind::Instantiates`] edge. See
/// `docs/development/go-channels-and-generic-instantiation/02_DESIGN.md` §3.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceKind {
    /// All type arguments were explicit (`Map[string, int](...)`).
    Explicit,
    /// All type arguments were inferred from function-argument types.
    Inferred,
    /// Explicit prefix + inferred / unknown suffix (the boldlygo.tech
    /// "right-to-left omission" subset). `apply[[]int](nil, f)`.
    Partial,
    /// One or more slots were unsolvable by Phase 1 rules and recorded as
    /// the `<unknown>` sentinel.
    Unknown,
}

/// One slot in a generic instantiation's type-argument vector (Go T2.5).
///
/// `Copy` and 8 bytes (4-byte `StringId` + 1-byte bool + 3 padding) so a
/// `SmallVec<[TypeArg; 4]>` inlines its common case on the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypeArg {
    /// Interned type-name string. The exact string `"<unknown>"` for
    /// unresolved slots (no separate sentinel discriminant — see §4.4).
    pub name: StringId,
    /// True when the slot was filled by Go's untyped-constant default rule
    /// (`int` for untyped int, `float64` for untyped float, etc. — AC-10).
    /// Always `false` for the `<unknown>` sentinel.
    pub default_typed: bool,
}

/// Enumeration of edge relationship types in the graph.
///
/// Each variant represents a distinct kind of relationship between nodes.
/// The categorization is language-agnostic to support cross-language analysis.
///
/// Note: Uses default externally-tagged enum representation for serialization compatibility.
/// JSON output will be `{"calls": {"argument_count": 0, "is_async": false}}` rather than
/// `{"type": "calls", "argument_count": 0, "is_async": false}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    // ==================== Structural ====================
    /// A symbol defines another (e.g., module defines function).
    Defines,

    /// A container contains another (e.g., class contains method).
    Contains,

    // ==================== References ====================
    /// A function/method calls another.
    Calls {
        /// Number of arguments in the call (0-255)
        argument_count: u8,
        /// Whether the call expression is directly awaited (uses `.await`).
        ///
        /// This indicates an *awaited call site*, not merely "inside an async function".
        is_async: bool,
        /// How this call edge's target was resolved (DESIGN §6).
        ///
        /// New in Phase A. `#[serde(default)]` keeps V10 **JSON / key-value**
        /// wire payloads (which can omit this field) decodable with
        /// `ResolvedVia::Direct`. Postcard is positional and cannot rescue a
        /// trailing-field absence — V10 postcard `Calls` bytes are decoded by
        /// the snapshot persistence layer's explicit V10 reader path
        /// (`sqry-core/src/graph/unified/persistence/snapshot.rs::upconvert_v10_to_v11`,
        /// added by U03), NOT by this serde annotation.
        #[serde(default)]
        resolved_via: ResolvedVia,
    },

    /// A symbol references another (read access).
    References,

    /// An import statement brings in a symbol.
    Imports {
        /// Optional alias for the import (e.g., `import { foo as bar }`)
        alias: Option<StringId>,
        /// Whether this is a wildcard import (e.g., `import *`)
        is_wildcard: bool,
    },

    /// An export statement exposes a symbol.
    Exports {
        /// The kind of export (direct, re-export, default, namespace)
        kind: ExportKind,
        /// Optional alias for the export (e.g., `export { foo as bar }`)
        alias: Option<StringId>,
    },

    /// A type reference with optional context metadata.
    ///
    /// Represents relationships like:
    /// - Function parameter types (`context = Parameter`)
    /// - Function return types (`context = Return`)
    /// - Variable types (`context = Variable`)
    /// - Struct field types (`context = Field`)
    ///
    /// The `context`, `index`, and `name` fields provide semantic information
    /// about where and how the type reference appears.
    TypeOf {
        /// Context where this type reference appears
        context: Option<TypeOfContext>,
        /// Position/index (for parameters, returns, fields)
        index: Option<u16>,
        /// Name (for parameters, returns, fields, variables)
        name: Option<StringId>,
    },

    // ==================== OOP ====================
    /// A class inherits from another (extends).
    Inherits,

    /// A class/struct implements an interface/trait.
    Implements,

    // ==================== Rust-Specific ====================
    /// Lifetime constraint relationship.
    ///
    /// Models Rust lifetime bounds. Source and target are `NodeKind::Lifetime`
    /// or `NodeKind::Type` nodes (for type bounds like `T: 'a`).
    ///
    /// Query semantics: NOT included in `callers/callees` results.
    /// Use dedicated query: `--lifetime-constraints`
    LifetimeConstraint {
        /// The kind of lifetime constraint
        constraint_kind: LifetimeConstraintKind,
    },

    /// Trait method binding (call site -> impl method).
    ///
    /// Represents the resolution of a trait method call to a concrete
    /// implementation. This is distinct from `Calls` because it involves
    /// trait method resolution logic.
    ///
    /// Query semantics: NOT included in `callers` by default.
    /// Use `--include-trait-bindings` flag or dedicated query.
    TraitMethodBinding {
        /// The trait providing the method
        trait_name: StringId,
        /// The implementing type
        impl_type: StringId,
        /// Whether this binding is ambiguous (multiple impls match)
        is_ambiguous: bool,
    },

    /// Macro expansion relationship.
    ///
    /// Represents the expansion of a macro invocation to its generated code.
    /// Only available when macro expansion is enabled (security opt-in).
    ///
    /// Query semantics: Included in `callees` when expansion enabled.
    MacroExpansion {
        /// The kind of macro expansion
        expansion_kind: MacroExpansionKind,
        /// Whether the expansion has been verified against source
        is_verified: bool,
    },

    // ==================== Cross-language / Cross-service ====================
    /// Foreign function interface call.
    FfiCall {
        /// The calling convention used
        convention: FfiConvention,
    },

    /// HTTP request to an endpoint.
    HttpRequest {
        /// The HTTP method
        method: HttpMethod,
        /// Optional URL pattern
        url: Option<StringId>,
    },

    /// gRPC service call.
    GrpcCall {
        /// Service name
        service: StringId,
        /// Method name
        method: StringId,
    },

    /// WebAssembly function call.
    WebAssemblyCall,

    /// Database query execution.
    DbQuery {
        /// Query type
        query_type: DbQueryType,
        /// Optional table/collection name
        table: Option<StringId>,
    },

    /// Database table read operation (SQL).
    TableRead {
        /// Name of the table being read
        table_name: StringId,
        /// Optional schema/database name
        schema: Option<StringId>,
    },

    /// Database table write operation (SQL).
    TableWrite {
        /// Name of the table being written
        table_name: StringId,
        /// Optional schema/database name
        schema: Option<StringId>,
        /// Type of write operation (INSERT/UPDATE/DELETE)
        operation: TableWriteOp,
    },

    /// Database trigger relationship (SQL).
    TriggeredBy {
        /// Name of the trigger
        trigger_name: StringId,
        /// Optional schema/database name
        schema: Option<StringId>,
    },

    // ==================== Extended ====================
    /// Message queue publish/subscribe.
    MessageQueue {
        /// Protocol used
        protocol: MqProtocol,
        /// Optional topic/queue name
        topic: Option<StringId>,
    },

    /// WebSocket event communication.
    WebSocket {
        /// Optional event name
        event: Option<StringId>,
    },

    /// GraphQL operation (query/mutation/subscription).
    GraphQLOperation {
        /// Operation name
        operation: StringId,
    },

    /// Process execution (spawn, exec).
    ProcessExec {
        /// Command being executed
        command: StringId,
    },

    /// File-based IPC (pipes, shared memory, temp files).
    FileIpc {
        /// Optional path pattern
        path_pattern: Option<StringId>,
    },

    // ==================== Extensibility ====================
    /// Generic protocol call for extensibility.
    ProtocolCall {
        /// Protocol identifier
        protocol: StringId,
        /// Optional JSON-encoded metadata
        metadata: Option<StringId>,
    },

    // ==================== JVM Classpath (Track C) ====================
    /// Generic type bound (e.g., `T extends Comparable<T>`).
    GenericBound,

    /// Symbol annotated with annotation type.
    AnnotatedWith,

    /// Annotation parameter binding (annotation -> element value).
    AnnotationParam,

    /// Lambda captures a method reference target.
    LambdaCaptures,

    /// Java module exports a package.
    ModuleExports,

    /// Java module requires another module.
    ModuleRequires,

    /// Java module opens a package for reflection.
    ModuleOpens,

    /// Java module provides a service implementation.
    ModuleProvides,

    /// Generic type argument (e.g., `String` in `List<String>`).
    TypeArgument,

    /// Kotlin extension function receiver type.
    ExtensionReceiver,

    /// Kotlin companion object relationship.
    CompanionOf,

    /// Kotlin sealed class permits a subclass.
    SealedPermit,

    // ==================== T3: Go error chains ====================
    /// Error-wrapping relationship between a wrapper expression and a
    /// wrapped error value.
    ///
    /// Emitted by Go plugin (T3.6) for `fmt.Errorf("%w", err)`, `Unwrap()`
    /// method bodies, and the `errors.{Is,As,AsType,Join}` family. The
    /// `kind` field identifies the source syntax; `chain_position` carries
    /// the verb index for `%w` and the slice index for `errors.Join` /
    /// `Unwrap() []error` slice literals (`None` for forms that do not
    /// have a meaningful position).
    ///
    /// Query semantics: NOT included in `callers/callees` results.
    /// Wrap-chain traversal lands later in T3 (planner `wraps:` predicate
    /// in Cluster F; `relation_query`/dedicated tooling in Cluster G);
    /// callers must walk `Wraps` edges explicitly until those surfaces
    /// land. The MCP `context_propagation` tool (T3.7) is a separate
    /// derived-cache query over span-resolved propagation chains, NOT
    /// a wrap-edge traversal surface.
    Wraps {
        /// The source-syntax form that authored this edge.
        kind: WrapKind,
        /// Optional position within the wrap chain — verb index for
        /// `%w` (0-based, skipping `%%`), slice index for `Unwrap()
        /// []error` slice literals and `errors.Join` variadic args,
        /// `None` for single-value forms.
        chain_position: Option<u16>,
    },

    // ==================== T2.4: Go channel pairing ====================
    /// Channel send / receive / close peer edge (Go T2.4).
    ///
    /// Edge **source**: a [`super::super::node::kind::NodeKind::CallSite`]
    /// representing the operation site (the `ch <- v` send-statement node,
    /// the `<-ch` unary-expr node, the `range ch` clause, the
    /// `case ch <- v:` / `case <-ch:` select arm, or the `close(ch)`
    /// builtin-call node).
    ///
    /// Edge **target**: a [`super::super::node::kind::NodeKind::Channel`]
    /// representing the alias-class of the channel.
    ///
    /// Multiple edges per channel are expected — one per operation site.
    /// `trace_path` walks send→channel←receive in two hops; consumers that
    /// want a one-hop view filter by `direction` on both edges.
    ///
    /// Appended after the current terminal `Wraps` (T3 #279) so all existing
    /// variant indices, including `Wraps`, are preserved on the postcard
    /// wire. Rides the V13→V14 snapshot bump driven by the `NodeKind`
    /// change (persistence §6.1).
    ChannelPeer {
        /// Whether this operation sends, receives, or closes.
        direction: ChannelPeerDirection,
        /// Cached classifier from the `Channel` node, replicated on the
        /// edge so the planner can filter without joining through the
        /// channel node.
        buffer_kind: ChannelBufferKind,
    },

    // ==================== T2.5: Generic instantiation ====================
    /// Generic-function call-site instantiation (Go T2.5; reusable for
    /// Rust / TS / Java in later phases).
    ///
    /// Edge **source**: a [`super::super::node::kind::NodeKind::CallSite`]
    /// for the generic call. Edge **target**: the generic function / method
    /// definition.
    ///
    /// The edge **co-exists** with the existing `Calls` edge at the same
    /// call site (AC-12 requires the `Calls` edge unchanged in every case);
    /// the `Calls` edge carries `argument_count` and `is_async`, the
    /// `Instantiates` edge carries the type-argument vector.
    Instantiates {
        /// Type arguments in declaration order. Each slot is a resolved
        /// type name or the interned `"<unknown>"` sentinel.
        /// `SmallVec<[TypeArg; 4]>` keeps the common 1-4-arg case on the
        /// stack (most Go generics are 1-2 type parameters).
        type_args: SmallVec<[TypeArg; 4]>,
        /// Discriminator on how the type-arg vector was derived.
        inference_kind: InferenceKind,
    },
}

impl EdgeKind {
    /// Returns `true` if this edge represents a function call relationship.
    #[inline]
    #[must_use]
    pub const fn is_call(&self) -> bool {
        matches!(
            self,
            Self::Calls { .. }
                | Self::FfiCall { .. }
                | Self::HttpRequest { .. }
                | Self::GrpcCall { .. }
                | Self::WebAssemblyCall
        )
    }

    /// Returns `true` if this edge represents a structural relationship.
    #[inline]
    #[must_use]
    pub const fn is_structural(&self) -> bool {
        matches!(self, Self::Defines | Self::Contains)
    }

    /// Returns `true` if this edge represents a type relationship.
    #[inline]
    #[must_use]
    pub const fn is_type_relation(&self) -> bool {
        matches!(
            self,
            Self::Inherits
                | Self::Implements
                | Self::TypeOf { .. }
                | Self::GenericBound
                | Self::TypeArgument
                | Self::ExtensionReceiver
                | Self::SealedPermit
        )
    }

    /// Returns `true` if this edge represents a cross-language/service boundary.
    #[inline]
    #[must_use]
    pub const fn is_cross_boundary(&self) -> bool {
        matches!(
            self,
            Self::FfiCall { .. }
                | Self::HttpRequest { .. }
                | Self::GrpcCall { .. }
                | Self::WebAssemblyCall
                | Self::DbQuery { .. }
                | Self::TableRead { .. }
                | Self::TableWrite { .. }
                | Self::TriggeredBy { .. }
                | Self::MessageQueue { .. }
                | Self::WebSocket { .. }
                | Self::GraphQLOperation { .. }
                | Self::ProcessExec { .. }
                | Self::FileIpc { .. }
                | Self::ProtocolCall { .. }
        )
    }

    /// Returns `true` if this is an async/message-based relationship.
    #[inline]
    #[must_use]
    pub const fn is_async(&self) -> bool {
        matches!(
            self,
            Self::MessageQueue { .. } | Self::WebSocket { .. } | Self::GraphQLOperation { .. }
        )
    }

    /// Returns `true` if this is a Rust-specific edge kind.
    ///
    /// These edges are produced by the Rust language plugin and have
    /// specialized query semantics.
    #[inline]
    #[must_use]
    pub const fn is_rust_specific(&self) -> bool {
        matches!(
            self,
            Self::LifetimeConstraint { .. }
                | Self::TraitMethodBinding { .. }
                | Self::MacroExpansion { .. }
        )
    }

    /// Returns `true` if this is a lifetime constraint edge.
    #[inline]
    #[must_use]
    pub const fn is_lifetime_constraint(&self) -> bool {
        matches!(self, Self::LifetimeConstraint { .. })
    }

    /// Returns `true` if this is a trait method binding edge.
    #[inline]
    #[must_use]
    pub const fn is_trait_method_binding(&self) -> bool {
        matches!(self, Self::TraitMethodBinding { .. })
    }

    /// Returns `true` if this is a macro expansion edge.
    #[inline]
    #[must_use]
    pub const fn is_macro_expansion(&self) -> bool {
        matches!(self, Self::MacroExpansion { .. })
    }

    /// Returns the canonical tag name for this edge kind.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Defines => "defines",
            Self::Contains => "contains",
            Self::Calls { .. } => "calls",
            Self::References => "references",
            Self::Imports { .. } => "imports",
            Self::Exports { .. } => "exports",
            Self::TypeOf { .. } => "type_of",
            Self::Inherits => "inherits",
            Self::Implements => "implements",
            Self::LifetimeConstraint { .. } => "lifetime_constraint",
            Self::TraitMethodBinding { .. } => "trait_method_binding",
            Self::MacroExpansion { .. } => "macro_expansion",
            Self::FfiCall { .. } => "ffi_call",
            Self::HttpRequest { .. } => "http_request",
            Self::GrpcCall { .. } => "grpc_call",
            Self::WebAssemblyCall => "web_assembly_call",
            Self::DbQuery { .. } => "db_query",
            Self::TableRead { .. } => "table_read",
            Self::TableWrite { .. } => "table_write",
            Self::TriggeredBy { .. } => "triggered_by",
            Self::MessageQueue { .. } => "message_queue",
            Self::WebSocket { .. } => "web_socket",
            Self::GraphQLOperation { .. } => "graphql_operation",
            Self::ProcessExec { .. } => "process_exec",
            Self::FileIpc { .. } => "file_ipc",
            Self::ProtocolCall { .. } => "protocol_call",
            Self::GenericBound => "generic_bound",
            Self::AnnotatedWith => "annotated_with",
            Self::AnnotationParam => "annotation_param",
            Self::LambdaCaptures => "lambda_captures",
            Self::ModuleExports => "module_exports",
            Self::ModuleRequires => "module_requires",
            Self::ModuleOpens => "module_opens",
            Self::ModuleProvides => "module_provides",
            Self::TypeArgument => "type_argument",
            Self::ExtensionReceiver => "extension_receiver",
            Self::CompanionOf => "companion_of",
            Self::SealedPermit => "sealed_permit",
            Self::Wraps { .. } => "wraps",
            Self::ChannelPeer { .. } => "channel_peer",
            Self::Instantiates { .. } => "instantiates",
        }
    }

    /// Returns an estimated byte size for this edge kind variant.
    ///
    /// Used for byte-level admission control in the delta buffer.
    /// Estimates are conservative approximations based on variant data.
    ///
    /// Not `const fn`: the `Instantiates` arm reads `type_args.len()`
    /// (`SmallVec::len` is not const). The only caller
    /// (`EdgeDelta::size`) is a runtime path.
    #[must_use]
    pub fn estimated_size(&self) -> usize {
        // Base enum discriminant: 1 byte
        // StringId: 4 bytes each
        // Option<StringId>: 5 bytes (1 discriminant + 4 payload)
        // bool: 1 byte, u8: 1 byte, ExportKind: 1 byte
        match self {
            // Unit variants: just discriminant
            Self::Defines
            | Self::Contains
            | Self::References
            | Self::Inherits
            | Self::Implements
            | Self::WebAssemblyCall
            | Self::GenericBound
            | Self::AnnotatedWith
            | Self::AnnotationParam
            | Self::LambdaCaptures
            | Self::ModuleExports
            | Self::ModuleRequires
            | Self::ModuleOpens
            | Self::ModuleProvides
            | Self::TypeArgument
            | Self::ExtensionReceiver
            | Self::CompanionOf
            | Self::SealedPermit => 1,

            // u8 + bool: 1 + 1 + 1
            // MacroExpansionKind + bool: 1 + 1
            // ChannelPeerDirection + ChannelBufferKind: 1 + 1
            Self::Calls { .. } | Self::MacroExpansion { .. } | Self::ChannelPeer { .. } => 3,

            // Option<StringId> + bool: 5 + 1 + 1 (imports/exports)
            // DbQueryType + Option<StringId>: 1 + 5
            Self::Imports { .. } | Self::Exports { .. } | Self::DbQuery { .. } => 7,

            // FfiConvention: 1 byte
            // LifetimeConstraintKind: 1 byte
            Self::FfiCall { .. } | Self::LifetimeConstraint { .. } => 2,

            // StringId + StringId + bool: 4 + 4 + 1
            // StringId + Option<StringId>: 4 + 5
            Self::TraitMethodBinding { .. }
            | Self::TableRead { .. }
            | Self::TriggeredBy { .. }
            | Self::ProtocolCall { .. } => 10,

            // HttpMethod + Option<StringId>: 1 + 5
            // Option<StringId>: 5 (websocket/file IPC)
            Self::HttpRequest { .. } | Self::WebSocket { .. } | Self::FileIpc { .. } => 6,

            // Two StringIds: 4 + 4
            Self::GrpcCall { .. } => 9,

            // StringId + Option<StringId> + TableWriteOp: 4 + 5 + 1
            // Option<TypeOfContext> + Option<u16> + Option<StringId>: 2 + 3 + 5
            Self::TableWrite { .. } | Self::MessageQueue { .. } | Self::TypeOf { .. } => 11,

            // StringId: 4
            Self::GraphQLOperation { .. } | Self::ProcessExec { .. } => 5,

            // WrapKind: 1 + Option<u16>: 3 = 4
            Self::Wraps { .. } => 4,

            // discriminant + len + N*(StringId + bool) + InferenceKind:
            // 1 + 4 + (len * 5) + 1
            Self::Instantiates { type_args, .. } => 6 + type_args.len() * 5,
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

impl Default for EdgeKind {
    /// Returns `EdgeKind::Calls` as the default (most common edge type).
    fn default() -> Self {
        Self::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to create a default Calls variant for tests.
    fn calls() -> EdgeKind {
        EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        }
    }

    /// Helper to create a default Imports variant for tests.
    fn imports() -> EdgeKind {
        EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        }
    }

    /// Helper to create a default Exports variant for tests.
    fn exports() -> EdgeKind {
        EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        }
    }

    #[test]
    fn test_edge_kind_tag() {
        assert_eq!(calls().tag(), "calls");
        assert_eq!(imports().tag(), "imports");
        assert_eq!(exports().tag(), "exports");
        assert_eq!(EdgeKind::Defines.tag(), "defines");
        assert_eq!(
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: None
            }
            .tag(),
            "http_request"
        );
    }

    #[test]
    fn test_edge_kind_display() {
        assert_eq!(format!("{}", calls()), "calls");
        assert_eq!(format!("{}", imports()), "imports");
        assert_eq!(format!("{}", exports()), "exports");
        assert_eq!(format!("{}", EdgeKind::Inherits), "inherits");
    }

    #[test]
    fn test_is_call() {
        assert!(calls().is_call());
        assert!(
            EdgeKind::Calls {
                argument_count: 5,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            }
            .is_call()
        );
        assert!(
            EdgeKind::FfiCall {
                convention: FfiConvention::C
            }
            .is_call()
        );
        assert!(
            EdgeKind::HttpRequest {
                method: HttpMethod::Post,
                url: None
            }
            .is_call()
        );
        assert!(!EdgeKind::Defines.is_call());
        assert!(!EdgeKind::Inherits.is_call());
        assert!(!imports().is_call());
        assert!(!exports().is_call());
    }

    #[test]
    fn test_is_structural() {
        assert!(EdgeKind::Defines.is_structural());
        assert!(EdgeKind::Contains.is_structural());
        assert!(!calls().is_structural());
        assert!(!imports().is_structural());
        assert!(!exports().is_structural());
    }

    #[test]
    fn test_is_type_relation() {
        assert!(EdgeKind::Inherits.is_type_relation());
        assert!(EdgeKind::Implements.is_type_relation());
        assert!(
            EdgeKind::TypeOf {
                context: None,
                index: None,
                name: None,
            }
            .is_type_relation()
        );
        assert!(!calls().is_type_relation());
    }

    #[test]
    fn test_is_cross_boundary() {
        assert!(
            EdgeKind::FfiCall {
                convention: FfiConvention::C
            }
            .is_cross_boundary()
        );
        assert!(
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: None
            }
            .is_cross_boundary()
        );
        assert!(
            EdgeKind::GrpcCall {
                service: StringId::INVALID,
                method: StringId::INVALID
            }
            .is_cross_boundary()
        );
        assert!(!calls().is_cross_boundary());
        assert!(!imports().is_cross_boundary());
        assert!(!exports().is_cross_boundary());
    }

    #[test]
    fn test_is_async() {
        assert!(
            EdgeKind::MessageQueue {
                protocol: MqProtocol::Kafka,
                topic: None
            }
            .is_async()
        );
        assert!(EdgeKind::WebSocket { event: None }.is_async());
        assert!(!calls().is_async());
        // Note: EdgeKind::Calls with is_async: true still returns false from is_async()
        // because is_async() refers to async communication patterns, not async function calls
        assert!(
            !EdgeKind::Calls {
                argument_count: 0,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            }
            .is_async()
        );
    }

    #[test]
    fn test_default() {
        assert_eq!(EdgeKind::default(), calls());
        assert_eq!(HttpMethod::default(), HttpMethod::Get);
        assert_eq!(FfiConvention::default(), FfiConvention::C);
        assert_eq!(DbQueryType::default(), DbQueryType::Select);
        assert_eq!(ExportKind::default(), ExportKind::Direct);
    }

    #[test]
    fn test_http_method_as_str() {
        assert_eq!(HttpMethod::Get.as_str(), "GET");
        assert_eq!(HttpMethod::Post.as_str(), "POST");
        assert_eq!(HttpMethod::Delete.as_str(), "DELETE");
        assert_eq!(HttpMethod::All.as_str(), "ALL");
    }

    #[test]
    fn test_calls_with_metadata() {
        let sync_call = EdgeKind::Calls {
            argument_count: 3,
            is_async: false,
            resolved_via: ResolvedVia::Direct,
        };
        let async_call = EdgeKind::Calls {
            argument_count: 0,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        assert_eq!(sync_call.tag(), "calls");
        assert_eq!(async_call.tag(), "calls");
        assert!(sync_call.is_call());
        assert!(async_call.is_call());
        assert_ne!(sync_call, async_call);
    }

    #[test]
    fn test_imports_with_metadata() {
        let simple = imports();
        let aliased = EdgeKind::Imports {
            alias: Some(StringId::new(42)),
            is_wildcard: false,
        };
        let wildcard = EdgeKind::Imports {
            alias: None,
            is_wildcard: true,
        };

        assert_eq!(simple.tag(), "imports");
        assert_eq!(aliased.tag(), "imports");
        assert_eq!(wildcard.tag(), "imports");
        assert_ne!(simple, aliased);
        assert_ne!(simple, wildcard);
    }

    #[test]
    fn test_exports_with_metadata() {
        let direct = exports();
        let reexport = EdgeKind::Exports {
            kind: ExportKind::Reexport,
            alias: None,
        };
        let default_export = EdgeKind::Exports {
            kind: ExportKind::Default,
            alias: None,
        };
        let namespace = EdgeKind::Exports {
            kind: ExportKind::Namespace,
            alias: Some(StringId::new(1)),
        };

        assert_eq!(direct.tag(), "exports");
        assert_eq!(reexport.tag(), "exports");
        assert_eq!(default_export.tag(), "exports");
        assert_eq!(namespace.tag(), "exports");
        assert_ne!(direct, reexport);
        assert_ne!(direct, default_export);
    }

    #[test]
    fn test_serde_calls_imports_exports() {
        // Calls with metadata
        let calls = EdgeKind::Calls {
            argument_count: 5,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        let json = serde_json::to_string(&calls).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(calls, deserialized);
        assert!(json.contains("\"calls\""));
        assert!(json.contains("\"argument_count\":5"));
        assert!(json.contains("\"is_async\":true"));

        // Imports with alias
        let imports = EdgeKind::Imports {
            alias: Some(StringId::new(10)),
            is_wildcard: false,
        };
        let json = serde_json::to_string(&imports).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(imports, deserialized);

        // Exports with kind
        let exports = EdgeKind::Exports {
            kind: ExportKind::Reexport,
            alias: None,
        };
        let json = serde_json::to_string(&exports).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(exports, deserialized);
    }

    #[test]
    fn test_serde_complex_variants() {
        // HttpRequest with fields
        let http = EdgeKind::HttpRequest {
            method: HttpMethod::Post,
            url: None,
        };
        let json = serde_json::to_string(&http).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(http, deserialized);

        // GrpcCall with StringIds
        let grpc = EdgeKind::GrpcCall {
            service: StringId::new(1),
            method: StringId::new(2),
        };
        let json = serde_json::to_string(&grpc).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(grpc, deserialized);
    }

    #[test]
    fn test_postcard_roundtrip_simple_enums() {
        // Test postcard roundtrip for component enums used by EdgeKind.

        // FfiConvention
        for conv in [
            FfiConvention::C,
            FfiConvention::Cdecl,
            FfiConvention::Stdcall,
        ] {
            let bytes = postcard::to_allocvec(&conv).unwrap();
            let deserialized: FfiConvention = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(conv, deserialized);
        }

        // HttpMethod
        for method in [
            HttpMethod::Get,
            HttpMethod::Post,
            HttpMethod::Delete,
            HttpMethod::All,
        ] {
            let bytes = postcard::to_allocvec(&method).unwrap();
            let deserialized: HttpMethod = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(method, deserialized);
        }

        // DbQueryType
        for query in [
            DbQueryType::Select,
            DbQueryType::Insert,
            DbQueryType::Update,
        ] {
            let bytes = postcard::to_allocvec(&query).unwrap();
            let deserialized: DbQueryType = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(query, deserialized);
        }

        // ExportKind
        for kind in [
            ExportKind::Direct,
            ExportKind::Reexport,
            ExportKind::Default,
            ExportKind::Namespace,
        ] {
            let bytes = postcard::to_allocvec(&kind).unwrap();
            let deserialized: ExportKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(kind, deserialized);
        }
    }

    #[test]
    fn test_edge_kind_json_compatibility() {
        // EdgeKind is designed for JSON serialization (MCP export).
        // Binary persistence in Phase 6 will use a custom format.
        let kinds = [
            calls(),
            imports(),
            exports(),
            EdgeKind::Defines,
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: None,
            },
            EdgeKind::MessageQueue {
                protocol: MqProtocol::Kafka,
                topic: Some(StringId::new(1)),
            },
        ];

        for kind in &kinds {
            // JSON roundtrip should work
            let json = serde_json::to_string(kind).unwrap();
            let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, deserialized);

            // Postcard roundtrip should also work (required for graph persistence)
            let bytes = postcard::to_allocvec(kind).unwrap();
            let from_postcard: EdgeKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(*kind, from_postcard);
        }
    }

    #[test]
    fn test_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(calls());
        set.insert(imports());
        set.insert(exports());
        set.insert(EdgeKind::Defines);
        set.insert(EdgeKind::HttpRequest {
            method: HttpMethod::Get,
            url: None,
        });

        assert!(set.contains(&calls()));
        assert!(set.contains(&imports()));
        assert!(set.contains(&exports()));
        assert!(!set.contains(&EdgeKind::Inherits));
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn test_ffi_convention_variants() {
        let conventions = [
            FfiConvention::C,
            FfiConvention::Cdecl,
            FfiConvention::Stdcall,
            FfiConvention::Fastcall,
            FfiConvention::System,
        ];

        for conv in conventions {
            let edge = EdgeKind::FfiCall { convention: conv };
            assert!(edge.is_call());
            assert!(edge.is_cross_boundary());
        }
    }

    #[test]
    fn test_mq_protocol_variants() {
        let protocols = [
            MqProtocol::Kafka,
            MqProtocol::Sqs,
            MqProtocol::RabbitMq,
            MqProtocol::Nats,
            MqProtocol::Redis,
            MqProtocol::Other(StringId::new(1)),
        ];

        for proto in protocols {
            let edge = EdgeKind::MessageQueue {
                protocol: proto.clone(),
                topic: None,
            };
            assert!(edge.is_async());
            assert!(edge.is_cross_boundary());
        }
    }

    #[test]
    fn test_export_kind_variants() {
        let kinds = [
            ExportKind::Direct,
            ExportKind::Reexport,
            ExportKind::Default,
            ExportKind::Namespace,
        ];

        for kind in kinds {
            let edge = EdgeKind::Exports { kind, alias: None };
            assert_eq!(edge.tag(), "exports");
            assert!(!edge.is_call());
            assert!(!edge.is_structural());
            assert!(!edge.is_cross_boundary());
        }
    }

    #[test]
    fn test_estimated_size() {
        // Unit variants
        assert_eq!(EdgeKind::Defines.estimated_size(), 1);
        assert_eq!(EdgeKind::Contains.estimated_size(), 1);
        assert_eq!(EdgeKind::References.estimated_size(), 1);

        // Calls: u8 + bool = 3
        assert_eq!(calls().estimated_size(), 3);

        // Imports: Option<StringId> + bool = 7
        assert_eq!(imports().estimated_size(), 7);

        // Exports: ExportKind + Option<StringId> = 7
        assert_eq!(exports().estimated_size(), 7);

        // Rust-specific edges
        assert_eq!(
            EdgeKind::LifetimeConstraint {
                constraint_kind: LifetimeConstraintKind::Outlives
            }
            .estimated_size(),
            2
        );
        assert_eq!(
            EdgeKind::MacroExpansion {
                expansion_kind: MacroExpansionKind::Derive,
                is_verified: true
            }
            .estimated_size(),
            3
        );
        assert_eq!(
            EdgeKind::TraitMethodBinding {
                trait_name: StringId::INVALID,
                impl_type: StringId::INVALID,
                is_ambiguous: false
            }
            .estimated_size(),
            10
        );
    }

    // ==================== Rust-Specific Edge Tests ====================

    #[test]
    fn test_lifetime_constraint_kind_variants() {
        let kinds = [
            LifetimeConstraintKind::Outlives,
            LifetimeConstraintKind::TypeBound,
            LifetimeConstraintKind::Reference,
            LifetimeConstraintKind::Static,
            LifetimeConstraintKind::HigherRanked,
            LifetimeConstraintKind::TraitObject,
            LifetimeConstraintKind::ImplTrait,
            LifetimeConstraintKind::Elided,
        ];

        for constraint_kind in kinds {
            let edge = EdgeKind::LifetimeConstraint { constraint_kind };
            assert!(edge.is_rust_specific());
            assert!(edge.is_lifetime_constraint());
            assert!(!edge.is_call());
            assert!(!edge.is_structural());
            assert_eq!(edge.tag(), "lifetime_constraint");
        }
    }

    #[test]
    fn test_macro_expansion_kind_variants() {
        let kinds = [
            MacroExpansionKind::Derive,
            MacroExpansionKind::Attribute,
            MacroExpansionKind::Declarative,
            MacroExpansionKind::Function,
            MacroExpansionKind::CfgGate,
        ];

        for expansion_kind in kinds {
            let edge = EdgeKind::MacroExpansion {
                expansion_kind,
                is_verified: true,
            };
            assert!(edge.is_rust_specific());
            assert!(edge.is_macro_expansion());
            assert!(!edge.is_call());
            assert_eq!(edge.tag(), "macro_expansion");
        }
    }

    #[test]
    fn test_trait_method_binding() {
        let edge = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(1),
            impl_type: StringId::new(2),
            is_ambiguous: false,
        };

        assert!(edge.is_rust_specific());
        assert!(edge.is_trait_method_binding());
        assert!(!edge.is_call());
        assert_eq!(edge.tag(), "trait_method_binding");

        // Test ambiguous binding
        let ambiguous = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(1),
            impl_type: StringId::new(2),
            is_ambiguous: true,
        };
        assert!(ambiguous.is_trait_method_binding());
    }

    #[test]
    fn test_rust_specific_edges_serde() {
        // LifetimeConstraint
        let lifetime = EdgeKind::LifetimeConstraint {
            constraint_kind: LifetimeConstraintKind::HigherRanked,
        };
        let json = serde_json::to_string(&lifetime).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(lifetime, deserialized);

        // TraitMethodBinding
        let binding = EdgeKind::TraitMethodBinding {
            trait_name: StringId::new(10),
            impl_type: StringId::new(20),
            is_ambiguous: true,
        };
        let json = serde_json::to_string(&binding).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(binding, deserialized);

        // MacroExpansion
        let expansion = EdgeKind::MacroExpansion {
            expansion_kind: MacroExpansionKind::Derive,
            is_verified: false,
        };
        let json = serde_json::to_string(&expansion).unwrap();
        let deserialized: EdgeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(expansion, deserialized);
    }

    #[test]
    fn test_rust_specific_edges_postcard() {
        let edges = [
            EdgeKind::LifetimeConstraint {
                constraint_kind: LifetimeConstraintKind::Outlives,
            },
            EdgeKind::TraitMethodBinding {
                trait_name: StringId::new(5),
                impl_type: StringId::new(6),
                is_ambiguous: false,
            },
            EdgeKind::MacroExpansion {
                expansion_kind: MacroExpansionKind::Attribute,
                is_verified: true,
            },
        ];

        for edge in edges {
            let bytes = postcard::to_allocvec(&edge).unwrap();
            let deserialized: EdgeKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(edge, deserialized);
        }
    }

    #[test]
    fn test_lifetime_constraint_kind_defaults() {
        assert_eq!(
            LifetimeConstraintKind::default(),
            LifetimeConstraintKind::Outlives
        );
        assert_eq!(
            MacroExpansionKind::default(),
            MacroExpansionKind::Declarative
        );
    }

    #[test]
    fn wraps_edge_serde_roundtrip() {
        // T3 Cluster A — exercises postcard + serde_json roundtrip across
        // every WrapKind, with and without chain_position. Variant ordering
        // is wire-format-significant (postcard encodes enum discriminants by
        // declaration index); this test pins the seven variants in order.
        let wrap_kinds = [
            WrapKind::ErrorfVerb,
            WrapKind::UnwrapMethod,
            WrapKind::UnwrapMultiMethod,
            WrapKind::ErrorsIs,
            WrapKind::ErrorsAs,
            WrapKind::ErrorsAsType,
            WrapKind::ErrorsJoin,
        ];

        for kind in wrap_kinds {
            for chain_position in [None, Some(0u16), Some(7u16), Some(u16::MAX)] {
                let edge = EdgeKind::Wraps {
                    kind,
                    chain_position,
                };

                let bytes = postcard::to_allocvec(&edge).unwrap();
                let postcard_back: EdgeKind = postcard::from_bytes(&bytes).unwrap();
                assert_eq!(
                    edge, postcard_back,
                    "postcard roundtrip mismatch for kind={kind:?} chain_position={chain_position:?}"
                );

                let json = serde_json::to_string(&edge).unwrap();
                let json_back: EdgeKind = serde_json::from_str(&json).unwrap();
                assert_eq!(
                    edge, json_back,
                    "serde_json roundtrip mismatch for kind={kind:?} chain_position={chain_position:?}"
                );
                assert!(
                    json.contains("\"wraps\""),
                    "JSON encoding must use snake_case tag `wraps`: {json}"
                );
                // Pin the inner `WrapKind` snake_case spelling so
                // removing `#[serde(rename_all = "snake_case")]` from
                // WrapKind breaks this test. Without these asserts the
                // roundtrip alone would silently accept PascalCase
                // (deserialize accepts what serialize emits).
                let expected_kind_str = match kind {
                    WrapKind::ErrorfVerb => "errorf_verb",
                    WrapKind::UnwrapMethod => "unwrap_method",
                    WrapKind::UnwrapMultiMethod => "unwrap_multi_method",
                    WrapKind::ErrorsIs => "errors_is",
                    WrapKind::ErrorsAs => "errors_as",
                    WrapKind::ErrorsAsType => "errors_as_type",
                    WrapKind::ErrorsJoin => "errors_join",
                };
                assert!(
                    json.contains(&format!("\"{expected_kind_str}\"")),
                    "JSON encoding must carry snake_case WrapKind `{expected_kind_str}`: {json}"
                );
            }
        }

        // Default WrapKind must be the first variant (ErrorfVerb) — the
        // serialization-stability comment on WrapKind requires append-only
        // variant ordering, and `#[default]` is on ErrorfVerb.
        assert_eq!(WrapKind::default(), WrapKind::ErrorfVerb);

        // Tag is stable across all variants and chain positions.
        assert_eq!(
            EdgeKind::Wraps {
                kind: WrapKind::ErrorfVerb,
                chain_position: Some(0),
            }
            .tag(),
            "wraps"
        );
    }

    // ========================================================================
    // ResolvedVia tests (TEST:c-icall-precision-017)
    //
    // Cover the four acceptance criteria for U04_RESOLVED_VIA:
    //   1. ResolvedVia::default() == ResolvedVia::Direct
    //   2. serde rename_all = "snake_case" produces `direct` / `type_match` /
    //      `binding_plane` and round-trips for all three variants
    //   3. `#[serde(default)]` on Calls.resolved_via lets a V10-shape Calls
    //      payload (without `resolved_via` field) decode into V11 shape with
    //      `resolved_via == Direct` — covered for **JSON / key-value** formats
    //      only. Postcard old-wire forward-compat lives in
    //      `sqry-core/src/graph/unified/persistence/snapshot.rs::upconvert_v10_to_v11`
    //      (U03's explicit V10 reader path), NOT in this serde annotation.
    //   4. Full Calls round-trip preserves `resolved_via` non-default values
    //      end-to-end via JSON and postcard
    // ========================================================================

    /// `ResolvedVia::default()` must return `Direct` so pre-Phase-A `Calls`
    /// edges retain their semantic provenance without explicit construction.
    /// See DESIGN §6.1 and U04 critical decisions in the DAG.
    #[test]
    fn calls_edge_resolved_via_default_is_direct() {
        assert_eq!(ResolvedVia::default(), ResolvedVia::Direct);

        // An `EdgeKind::Calls` value constructed without an explicit
        // `resolved_via` (via field-default) also yields `Direct`.
        let kind = EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
            resolved_via: ResolvedVia::default(),
        };
        if let EdgeKind::Calls { resolved_via, .. } = kind {
            assert_eq!(resolved_via, ResolvedVia::Direct);
        } else {
            unreachable!("EdgeKind::Calls construction must be reachable");
        }
    }

    /// `#[serde(rename_all = "snake_case")]` must produce the three exact wire
    /// spellings the planner predicate parser depends on
    /// (`direct` / `type_match` / `binding_plane` — DESIGN §11.2).
    #[test]
    fn calls_edge_resolved_via_serde_snake_case_round_trip() {
        for (variant, wire) in [
            (ResolvedVia::Direct, "\"direct\""),
            (ResolvedVia::TypeMatch, "\"type_match\""),
            (ResolvedVia::BindingPlane, "\"binding_plane\""),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, wire, "ResolvedVia::{variant:?} serializes to {wire}");
            let parsed: ResolvedVia = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant, "round-trip for {wire}");
        }
    }

    /// `#[serde(default)]` on `EdgeKind::Calls.resolved_via` lets a V10-shape
    /// Calls payload (no `resolved_via` field) decode into the V11 shape with
    /// `resolved_via = ResolvedVia::Direct` **for key-value formats like JSON
    /// where field absence is expressible**. Postcard (the on-disk snapshot
    /// format) is a positional binary format with no "field absence" concept,
    /// so `#[serde(default)]` cannot rescue a V10-shape postcard payload —
    /// V10 postcard bytes for `Calls` end after `is_async` and decoding as
    /// V11 fails with "Hit the end of buffer, expected more data".
    ///
    /// V10 → V11 postcard forward-compat lives in the snapshot persistence
    /// layer (`sqry-core/src/graph/unified/persistence/snapshot.rs`,
    /// `upconvert_v10_to_v11`) via explicit V10 type translation, NOT via
    /// `#[serde(default)]` on this enum. This test is therefore scoped to
    /// the JSON path only — the formal V11 wire round-trip lives in U06.
    #[test]
    fn calls_edge_json_default_old_wire() {
        // Hand-craft a V10-shape Calls payload via a parallel struct that
        // serializes to the same wire format as pre-Phase-A `Calls` did.
        #[derive(Serialize)]
        #[serde(rename_all = "snake_case")]
        enum LegacyEdgeKind {
            #[serde(rename = "calls")]
            Calls { argument_count: u8, is_async: bool },
        }

        let legacy = LegacyEdgeKind::Calls {
            argument_count: 7,
            is_async: true,
        };

        // ---- JSON path ----
        let legacy_json = serde_json::to_string(&legacy).unwrap();
        // Sanity-check: the legacy payload literally omits `resolved_via`.
        assert!(!legacy_json.contains("resolved_via"));

        let decoded: EdgeKind = serde_json::from_str(&legacy_json).unwrap();
        match decoded {
            EdgeKind::Calls {
                argument_count,
                is_async,
                resolved_via,
            } => {
                assert_eq!(argument_count, 7);
                assert!(is_async);
                assert_eq!(resolved_via, ResolvedVia::Direct);
            }
            other => panic!("expected EdgeKind::Calls, got {other:?}"),
        }
    }

    /// Two `Calls` edges that differ only in `resolved_via` must be unequal
    /// — that's the planner's semantic-discriminator contract (DESIGN §6.3bis
    /// Mechanism A). Combined with full JSON + postcard round-trip, this also
    /// confirms `TypeMatch` and `BindingPlane` survive every wire path.
    #[test]
    fn calls_edge_resolved_via_distinguishes_variants_round_trip() {
        let direct = EdgeKind::Calls {
            argument_count: 2,
            is_async: true,
            resolved_via: ResolvedVia::Direct,
        };
        let type_match = EdgeKind::Calls {
            argument_count: 2,
            is_async: true,
            resolved_via: ResolvedVia::TypeMatch,
        };
        let binding_plane = EdgeKind::Calls {
            argument_count: 2,
            is_async: true,
            resolved_via: ResolvedVia::BindingPlane,
        };

        // Field-level discrimination — required so the planner's edge-kind
        // discriminator can fuse / dedup correctly per DESIGN §6.3bis.
        assert_ne!(direct, type_match);
        assert_ne!(direct, binding_plane);
        assert_ne!(type_match, binding_plane);
        // Same kind tag despite distinct `resolved_via` values.
        assert_eq!(direct.tag(), "calls");
        assert_eq!(type_match.tag(), "calls");
        assert_eq!(binding_plane.tag(), "calls");

        // JSON round-trip preserves every field including `resolved_via`.
        for edge in [&direct, &type_match, &binding_plane] {
            let json = serde_json::to_string(edge).unwrap();
            assert!(
                json.contains("\"resolved_via\":"),
                "non-default Calls must emit `resolved_via` on the wire: {json}"
            );
            let decoded: EdgeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(&decoded, edge);
        }

        // Postcard round-trip is the on-disk graph-snapshot format
        // (V10+ uses postcard for graph payloads — see persistence::snapshot).
        for edge in [&direct, &type_match, &binding_plane] {
            let bytes = postcard::to_allocvec(edge).unwrap();
            let decoded: EdgeKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(&decoded, edge);
        }
    }
}
