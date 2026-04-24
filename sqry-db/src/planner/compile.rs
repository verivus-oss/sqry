//! Fluent [`QueryBuilder`] for assembling [`QueryPlan`] trees.
//!
//! The builder is the user-facing entry point to the planner pipeline. It
//! accumulates a sequence of operator steps and materialises them into a
//! [`PlanNode::Chain`] when [`QueryBuilder::build`] is called. Validation is
//! performed at `build` time; structural mistakes (an empty builder, a
//! traversal with `max_depth = 0`, a chain whose first step depends on an
//! input set) surface as [`BuildError`] variants rather than panics, so
//! callers can recover from malformed user input gracefully.
//!
//! # Pipeline position
//!
//! ```text
//!   text syntax / builder API
//!         │
//!         ▼
//!     [ir]
//!         │
//!         ▼
//!   [compile]  ← THIS MODULE — DB10
//!         │
//!         ▼
//!     [fuse]   — DB11
//!         │
//!         ▼
//!   [execute]  — DB12
//! ```
//!
//! # Example
//!
//! ```rust
//! use sqry_core::graph::unified::edge::kind::EdgeKind;
//! use sqry_core::graph::unified::node::kind::NodeKind;
//! use sqry_db::planner::{Direction, Predicate, QueryBuilder};
//!
//! let plan = QueryBuilder::new()
//!     .scan(NodeKind::Function)
//!     .filter(Predicate::HasCaller)
//!     .traverse(
//!         Direction::Reverse,
//!         EdgeKind::Calls {
//!             argument_count: 0,
//!             is_async: false,
//!         },
//!         3,
//!     )
//!     .filter(Predicate::InFile("src/api/**".into()))
//!     .build()
//!     .expect("plan validates");
//!
//! assert_eq!(plan.operator_count(), 5); // chain + 4 steps
//! ```
//!
//! # Design references
//!
//! - Spec: `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md` (§3.2)
//! - DAG: `docs/superpowers/plans/2026-04-12-phase3-4-combined-implementation-dag.toml` (unit DB10)

use sqry_core::graph::unified::edge::kind::EdgeKind;
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::graph::unified::string::StringId;
use sqry_core::schema::Visibility;
use thiserror::Error;

use super::ir::{
    Direction, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan, StringPattern,
};

// ============================================================================
// Errors
// ============================================================================

/// Errors produced when a [`QueryBuilder`] fails to materialise a valid plan.
///
/// Every error reflects a structural invariant the executor relies on. None of
/// these errors abort the calling thread — they are returned via
/// [`QueryBuilder::build`] so the caller can adjust the user query and retry.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum BuildError {
    /// The builder has no steps. Calling [`QueryBuilder::build`] on a freshly
    /// constructed [`QueryBuilder::new`] without any other method calls
    /// produces this error.
    #[error("query builder is empty: at least one step is required before build()")]
    EmptyBuilder,

    /// The first step of the chain is not context-free.
    ///
    /// The chain executor walks steps from left to right, threading each step's
    /// output into the next step's input. The first step therefore cannot
    /// depend on an inherited input set — only [`PlanNode::NodeScan`] and
    /// [`PlanNode::SetOp`] satisfy that contract (see
    /// [`PlanNode::is_context_free`]).
    #[error(
        "first step is not context-free: chains must start with NodeScan or SetOp, not {first_kind:?}"
    )]
    FirstStepNotContextFree {
        /// Name of the offending step variant for diagnostic purposes
        /// (e.g. `"Filter"`, `"EdgeTraversal"`, `"Chain"`).
        first_kind: PlanNodeKind,
    },

    /// An [`PlanNode::EdgeTraversal`] step was added with `max_depth = 0`.
    ///
    /// `max_depth = 0` would yield an empty result set unconditionally — the
    /// executor short-circuits, so the plan is unambiguously a user error.
    /// The builder rejects it at construction time.
    #[error("traversal step has max_depth = 0: must be >= 1 to produce any output")]
    ZeroDepth,

    /// A nested sub-plan supplied to [`QueryBuilder::union`],
    /// [`QueryBuilder::intersect`], or [`QueryBuilder::difference`] failed
    /// the same context-free validation as a chain's first step.
    #[error(
        "set-op operand is not a valid sub-plan: {reason} (the operand must itself build cleanly)"
    )]
    InvalidSetOpOperand {
        /// Human-readable explanation lifted from the inner [`BuildError`].
        reason: String,
    },
}

/// Compact identifier for a [`PlanNode`] variant, used in [`BuildError`]
/// payloads so error messages stay readable without exposing the full enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlanNodeKind {
    /// Corresponds to [`PlanNode::NodeScan`].
    NodeScan,
    /// Corresponds to [`PlanNode::EdgeTraversal`].
    EdgeTraversal,
    /// Corresponds to [`PlanNode::Filter`].
    Filter,
    /// Corresponds to [`PlanNode::SetOp`].
    SetOp,
    /// Corresponds to [`PlanNode::Chain`].
    Chain,
}

impl PlanNodeKind {
    /// Returns the variant kind for a given [`PlanNode`].
    #[must_use]
    pub const fn of(node: &PlanNode) -> Self {
        match node {
            PlanNode::NodeScan { .. } => Self::NodeScan,
            PlanNode::EdgeTraversal { .. } => Self::EdgeTraversal,
            PlanNode::Filter { .. } => Self::Filter,
            PlanNode::SetOp { .. } => Self::SetOp,
            PlanNode::Chain { .. } => Self::Chain,
        }
    }
}

// ============================================================================
// Edge-kind normalization
// ============================================================================

/// Strip metadata from an [`EdgeKind`] so that two traversals over the same
/// logical relation produce identical plan hashes.
///
/// # Why this matters for fusion
///
/// The DB11 fuser merges [`PlanNode`] subtrees that compare equal under
/// [`PartialEq`]. The executor matches edges by `std::mem::discriminant` —
/// i.e. it ignores metadata fields like
/// `Calls { argument_count, is_async }`. If the builder admitted the user's
/// `argument_count` value into the IR, two semantically identical traversals
/// constructed with different concrete values (a common occurrence when the
/// user is asking "do callers exist?" without caring about argument shape)
/// would end up with distinct plan hashes, defeating fusion.
///
/// Normalisation distinguishes **semantic discriminators** (kept) from
/// **site metadata** (zeroed):
///
/// ## Site metadata — always zeroed
///
/// These fields describe *where* or *how* the edge appeared in source code,
/// not *what kind* of relationship it is. Two traversals that differ only in
/// these fields ask the same semantic question.
///
/// - numeric per-site counters (`Calls::argument_count`, `TypeOf::index`)
/// - per-site flags with no query-level meaning (`Calls::is_async`,
///   `TraitMethodBinding::is_ambiguous`, `MacroExpansion::is_verified`)
/// - every [`StringId`] field — interner handles are snapshot-dependent and
///   therefore cannot survive a plan that may be re-evaluated against a
///   different snapshot. Semantic filtering on the names these
///   [`StringId`]s reference belongs in a [`Predicate`] (e.g.
///   [`Predicate::Implements`] with a
///   [`PredicateValue::Pattern`]), not
///   in the edge kind itself.
///
/// ## Semantic discriminators — always preserved
///
/// These fields carve the edge space into distinct semantic categories.
/// Collapsing them would merge edges that the caller asked about separately.
///
/// | Variant | Preserved field | Why |
/// |---------|-----------------|-----|
/// | [`EdgeKind::TypeOf`] | `context: Option<TypeOfContext>` | Parameter vs Return vs Field vs Variable vs TypeParameter vs Constraint are distinct relationships. |
/// | [`EdgeKind::Exports`] | `kind: ExportKind` | Direct / Reexport / Default / Namespace are distinct export forms. |
/// | [`EdgeKind::Imports`] | `is_wildcard: bool` | A `use foo::*` wildcard import is a different relationship from a named import. |
/// | [`EdgeKind::LifetimeConstraint`] | `constraint_kind` | Outlives / TypeBound / Reference / Static / HigherRanked / TraitObject are distinct Rust lifetime relations. |
/// | [`EdgeKind::MacroExpansion`] | `expansion_kind` | `CfgGate`, `Derive`, `Attribute`, `Declarative`, and `Function` expansions have distinct query semantics. |
/// | [`EdgeKind::FfiCall`] | `convention` | C vs Cdecl vs Stdcall vs Fastcall vs System are distinct FFI edges. |
/// | [`EdgeKind::HttpRequest`] | `method` | GET / POST / PUT / DELETE / PATCH / HEAD are distinct HTTP edges. |
/// | [`EdgeKind::DbQuery`] | `query_type` | Select / Insert / Update / Delete / Execute are distinct DB edges. |
/// | [`EdgeKind::TableWrite`] | `operation` | Insert / Update / Delete are distinct write edges. |
/// | [`EdgeKind::MessageQueue`] | `protocol` | Kafka / SQS / RabbitMQ / NATS / Redis / Other are distinct MQ edges. |
///
/// ## Pass-through
///
/// Variants with **no metadata** or **only semantic fields** are returned
/// unchanged. This includes the structural edges (`Defines`, `Contains`,
/// `References`, `Inherits`, `Implements`, `WebAssemblyCall`), the JVM
/// classpath edges (`GenericBound`, `AnnotatedWith`, `AnnotationParam`,
/// `LambdaCaptures`, `ModuleExports`, `ModuleRequires`, `ModuleOpens`,
/// `ModuleProvides`, `TypeArgument`, `ExtensionReceiver`, `CompanionOf`,
/// `SealedPermit`), and variants whose sole payload is a semantic discriminator
/// ([`EdgeKind::LifetimeConstraint`], [`EdgeKind::FfiCall`]).
///
/// [`Predicate`]: super::ir::Predicate
/// [`Predicate::Implements`]: super::ir::Predicate::Implements
#[must_use]
pub fn normalize_edge_kind(kind: EdgeKind) -> EdgeKind {
    match kind {
        // ---- Metadata-free or entirely-semantic variants: pass through ----
        EdgeKind::Defines
        | EdgeKind::Contains
        | EdgeKind::References
        | EdgeKind::Inherits
        | EdgeKind::Implements
        | EdgeKind::WebAssemblyCall
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
        | EdgeKind::LifetimeConstraint { .. }
        | EdgeKind::FfiCall { .. } => kind,

        // ---- Pure site metadata: zero everything ----
        EdgeKind::Calls { .. } => EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        },

        // ---- Mixed: preserve semantic, drop site/StringId ----
        EdgeKind::Imports { is_wildcard, .. } => EdgeKind::Imports {
            alias: None,
            is_wildcard,
        },
        EdgeKind::Exports { kind, .. } => EdgeKind::Exports { kind, alias: None },
        EdgeKind::TypeOf { context, .. } => EdgeKind::TypeOf {
            context,
            index: None,
            name: None,
        },
        EdgeKind::MacroExpansion { expansion_kind, .. } => EdgeKind::MacroExpansion {
            expansion_kind,
            is_verified: false,
        },
        EdgeKind::HttpRequest { method, .. } => EdgeKind::HttpRequest { method, url: None },
        EdgeKind::DbQuery { query_type, .. } => EdgeKind::DbQuery {
            query_type,
            table: None,
        },
        EdgeKind::TableWrite { operation, .. } => EdgeKind::TableWrite {
            table_name: StringId::INVALID,
            schema: None,
            operation,
        },
        EdgeKind::MessageQueue { protocol, .. } => EdgeKind::MessageQueue {
            protocol,
            topic: None,
        },

        // ---- Entirely StringId / site metadata: zero everything ----
        EdgeKind::TraitMethodBinding { .. } => EdgeKind::TraitMethodBinding {
            trait_name: StringId::INVALID,
            impl_type: StringId::INVALID,
            is_ambiguous: false,
        },
        EdgeKind::GrpcCall { .. } => EdgeKind::GrpcCall {
            service: StringId::INVALID,
            method: StringId::INVALID,
        },
        EdgeKind::TableRead { .. } => EdgeKind::TableRead {
            table_name: StringId::INVALID,
            schema: None,
        },
        EdgeKind::TriggeredBy { .. } => EdgeKind::TriggeredBy {
            trigger_name: StringId::INVALID,
            schema: None,
        },
        EdgeKind::WebSocket { .. } => EdgeKind::WebSocket { event: None },
        EdgeKind::GraphQLOperation { .. } => EdgeKind::GraphQLOperation {
            operation: StringId::INVALID,
        },
        EdgeKind::ProcessExec { .. } => EdgeKind::ProcessExec {
            command: StringId::INVALID,
        },
        EdgeKind::FileIpc { .. } => EdgeKind::FileIpc { path_pattern: None },
        EdgeKind::ProtocolCall { .. } => EdgeKind::ProtocolCall {
            protocol: StringId::INVALID,
            metadata: None,
        },
    }
}

// ============================================================================
// QueryBuilder
// ============================================================================

/// Optional filters for [`QueryBuilder::scan_with`].
///
/// Mirrors the `NodeScan` payload but accepts every field as `Option`, so a
/// caller can fix a subset of filters (e.g. visibility only) without having to
/// supply placeholder values for the others.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanFilters {
    /// Optional kind filter (Function, Method, Class, ...).
    pub kind: Option<NodeKind>,
    /// Optional visibility filter (Public, Private).
    pub visibility: Option<Visibility>,
    /// Optional symbol-name pattern filter.
    pub name_pattern: Option<StringPattern>,
}

impl ScanFilters {
    /// Constructs an empty [`ScanFilters`] (equivalent to [`Default::default`]).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            kind: None,
            visibility: None,
            name_pattern: None,
        }
    }

    /// Sets the kind filter.
    #[must_use]
    pub fn with_kind(mut self, kind: NodeKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Sets the visibility filter.
    #[must_use]
    pub fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = Some(visibility);
        self
    }

    /// Sets the name-pattern filter.
    #[must_use]
    pub fn with_name_pattern(mut self, pattern: StringPattern) -> Self {
        self.name_pattern = Some(pattern);
        self
    }
}

/// Fluent builder that compiles a chain of operator method calls into a
/// [`QueryPlan`].
///
/// # Composition rules
///
/// 1. The first step appended must be context-free. [`Self::scan`],
///    [`Self::scan_with`], [`Self::scan_all`], [`Self::union`],
///    [`Self::intersect`], and [`Self::difference`] all satisfy that
///    contract; calling [`Self::filter`] or [`Self::traverse`] on an empty
///    builder produces a [`BuildError::FirstStepNotContextFree`] at
///    [`Self::build`] time.
/// 2. [`Self::union`], [`Self::intersect`], and [`Self::difference`] take an
///    arbitrary [`QueryPlan`] as the right-hand operand. The builder's
///    current accumulated state is folded into the left-hand operand. If the
///    builder is empty, the right-hand operand becomes the first step.
/// 3. [`Self::traverse`] normalises its [`EdgeKind`] argument so that the
///    plan hash is independent of metadata the executor ignores
///    (see [`normalize_edge_kind`]).
/// 4. [`Self::build`] always wraps the accumulated steps in a
///    [`PlanNode::Chain`], even when only a single step is present. This
///    keeps the plan shape uniform for the fuser and the executor; callers
///    that want the bare step can match against the resulting `Chain.steps`.
///
/// # Cloning
///
/// [`QueryBuilder`] is `Clone` so that an in-progress chain can be branched
/// — useful when constructing two related queries from a shared prefix:
///
/// ```rust
/// use sqry_core::graph::unified::node::kind::NodeKind;
/// use sqry_db::planner::{Predicate, QueryBuilder};
///
/// let prefix = QueryBuilder::new().scan(NodeKind::Function);
/// let with_callers = prefix.clone().filter(Predicate::HasCaller).build();
/// let with_callees = prefix.filter(Predicate::HasCallee).build();
/// assert!(with_callers.is_ok() && with_callees.is_ok());
/// ```
#[derive(Debug, Clone, Default)]
pub struct QueryBuilder {
    /// Accumulated operator steps in insertion order.
    steps: Vec<PlanNode>,
}

impl QueryBuilder {
    /// Constructs an empty builder.
    ///
    /// At least one of [`Self::scan`], [`Self::scan_with`], [`Self::scan_all`],
    /// [`Self::union`], [`Self::intersect`], or [`Self::difference`] must be
    /// called before [`Self::build`] for the resulting plan to validate.
    #[must_use]
    pub const fn new() -> Self {
        Self { steps: Vec::new() }
    }

    // ------------------------------------------------------------------
    // Scan: context-free starting steps
    // ------------------------------------------------------------------

    /// Appends a [`PlanNode::NodeScan`] filtered to the given [`NodeKind`].
    ///
    /// Equivalent to `.scan_with(ScanFilters::new().with_kind(kind))`.
    #[must_use]
    pub fn scan(mut self, kind: NodeKind) -> Self {
        self.steps.push(PlanNode::NodeScan {
            kind: Some(kind),
            visibility: None,
            name_pattern: None,
        });
        self
    }

    /// Appends a [`PlanNode::NodeScan`] with the given filter combination.
    #[must_use]
    pub fn scan_with(mut self, filters: ScanFilters) -> Self {
        let ScanFilters {
            kind,
            visibility,
            name_pattern,
        } = filters;
        self.steps.push(PlanNode::NodeScan {
            kind,
            visibility,
            name_pattern,
        });
        self
    }

    /// Appends an unfiltered [`PlanNode::NodeScan`] that matches every node in
    /// the graph.
    ///
    /// Use sparingly — this is an expensive operation on large snapshots.
    #[must_use]
    pub fn scan_all(mut self) -> Self {
        self.steps.push(PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        });
        self
    }

    // ------------------------------------------------------------------
    // Filter
    // ------------------------------------------------------------------

    /// Appends a [`PlanNode::Filter`] step with the given predicate.
    ///
    /// Every variant of [`Predicate`] is acceptable here, including the
    /// boolean combinators ([`Predicate::And`], [`Predicate::Or`],
    /// [`Predicate::Not`]) and value-bearing relation predicates with
    /// [`PredicateValue::Subquery`] payloads.
    #[must_use]
    pub fn filter(mut self, predicate: Predicate) -> Self {
        self.steps.push(PlanNode::Filter { predicate });
        self
    }

    /// Convenience wrapper that builds a [`Predicate::MatchesName`] filter
    /// from a [`StringPattern`] and appends it as a [`PlanNode::Filter`].
    #[must_use]
    pub fn filter_name(self, pattern: StringPattern) -> Self {
        self.filter(Predicate::MatchesName(pattern))
    }

    /// Convenience wrapper that builds a [`Predicate::InFile`] filter from a
    /// [`PathPattern`] (or anything convertible to one) and appends it.
    #[must_use]
    pub fn filter_in_file(self, path: impl Into<PathPattern>) -> Self {
        self.filter(Predicate::InFile(path.into()))
    }

    // ------------------------------------------------------------------
    // Traverse
    // ------------------------------------------------------------------

    /// Appends a [`PlanNode::EdgeTraversal`] step.
    ///
    /// The supplied [`EdgeKind`] is run through [`normalize_edge_kind`] so
    /// that two builders that pick the same edge kind but supply different
    /// per-edge metadata produce the same plan hash. See
    /// [`normalize_edge_kind`] for the rationale.
    ///
    /// Note that `max_depth == 0` is accepted here but rejected at
    /// [`Self::build`] time with [`BuildError::ZeroDepth`]. The builder defers
    /// validation so that callers can compose traversals freely without
    /// fallible intermediate steps.
    #[must_use]
    pub fn traverse(mut self, direction: Direction, edge_kind: EdgeKind, max_depth: u32) -> Self {
        self.steps.push(PlanNode::EdgeTraversal {
            direction,
            edge_kind: Some(normalize_edge_kind(edge_kind)),
            max_depth,
        });
        self
    }

    /// Appends a [`PlanNode::EdgeTraversal`] step that matches any edge kind.
    ///
    /// Useful for "follow any outgoing edge to depth N" queries (impact
    /// analysis, broad reachability).
    #[must_use]
    pub fn traverse_any(mut self, direction: Direction, max_depth: u32) -> Self {
        self.steps.push(PlanNode::EdgeTraversal {
            direction,
            edge_kind: None,
            max_depth,
        });
        self
    }

    // ------------------------------------------------------------------
    // Set operations
    // ------------------------------------------------------------------

    /// Combines the builder's current state with another plan via union.
    ///
    /// If the builder already holds steps, they are wrapped in a
    /// [`PlanNode::Chain`] and used as the left-hand operand. If the builder
    /// is empty, the right-hand operand becomes the sole context-free step.
    #[must_use]
    pub fn union(self, other: QueryPlan) -> Self {
        self.combine(super::ir::SetOperation::Union, other)
    }

    /// Combines the builder's current state with another plan via intersection.
    ///
    /// See [`Self::union`] for the composition semantics when the builder is
    /// non-empty vs empty.
    #[must_use]
    pub fn intersect(self, other: QueryPlan) -> Self {
        self.combine(super::ir::SetOperation::Intersect, other)
    }

    /// Combines the builder's current state with another plan via difference.
    ///
    /// `result = self \ other`. See [`Self::union`] for composition semantics.
    #[must_use]
    pub fn difference(self, other: QueryPlan) -> Self {
        self.combine(super::ir::SetOperation::Difference, other)
    }

    /// Internal helper for [`Self::union`], [`Self::intersect`],
    /// [`Self::difference`]. Folds the builder's current state into the left
    /// operand of a [`PlanNode::SetOp`] node and resets the builder so the
    /// `SetOp` becomes the new (single) context-free step.
    ///
    /// # Empty-builder identity
    ///
    /// When called on an empty builder, the right operand is adopted as the
    /// builder's contents directly, bypassing the `SetOp` wrapper —
    /// `SetOp(∅, X) ≡ X` for all three operations modelled here, and
    /// constructing a `SetOp` with a synthetic empty left would be
    /// semantically wrong (the executor has no "empty set" `PlanNode`). If
    /// the right operand's root is itself a [`PlanNode::Chain`], its steps
    /// are flattened into the builder so the resulting plan stays within
    /// the canonical "single Chain" shape that [`Self::build`] guarantees.
    fn combine(mut self, op: super::ir::SetOperation, other: QueryPlan) -> Self {
        let right = Box::new(other.root);
        if self.steps.is_empty() {
            adopt_into_steps(&mut self.steps, *right);
            return self;
        }

        let left: Box<PlanNode> = if self.steps.len() == 1 {
            Box::new(self.steps.pop().expect("len == 1"))
        } else {
            Box::new(PlanNode::Chain {
                steps: std::mem::take(&mut self.steps),
            })
        };

        self.steps.push(PlanNode::SetOp { op, left, right });
        self
    }

    // ------------------------------------------------------------------
    // Inspection
    // ------------------------------------------------------------------

    /// Returns the number of steps currently accumulated in the builder.
    ///
    /// Does not include nested operators inside [`PlanNode::SetOp`] or
    /// [`PlanNode::Chain`] payloads — only top-level steps that
    /// [`Self::build`] would emit.
    #[inline]
    #[must_use]
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Returns `true` if the builder has no steps yet.
    #[inline]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    // ------------------------------------------------------------------
    // Build
    // ------------------------------------------------------------------

    /// Materialises the accumulated steps into a [`QueryPlan`].
    ///
    /// # Validation
    ///
    /// 1. The builder must contain at least one step
    ///    ([`BuildError::EmptyBuilder`]).
    /// 2. The first step must be context-free
    ///    ([`BuildError::FirstStepNotContextFree`]).
    /// 3. No [`PlanNode::EdgeTraversal`] step may have `max_depth = 0`
    ///    ([`BuildError::ZeroDepth`]). This is checked recursively through
    ///    [`PlanNode::SetOp`] and [`PlanNode::Chain`] payloads, so a malformed
    ///    sub-plan supplied to [`Self::union`] is also caught.
    /// 4. Any [`PlanNode::SetOp`] node added by [`Self::union`] /
    ///    [`Self::intersect`] / [`Self::difference`] must have both operands
    ///    rooted at a context-free node ([`BuildError::InvalidSetOpOperand`]).
    ///
    /// # Output shape
    ///
    /// The returned [`QueryPlan`]'s root is always a [`PlanNode::Chain`].
    /// Single-step builders therefore produce a `Chain { steps: vec![one] }`
    /// rather than the bare `one`. This keeps the plan shape uniform for
    /// downstream fusion and execution. Callers that prefer the bare step
    /// can match against `plan.root` and unwrap when `Chain.steps.len() == 1`.
    pub fn build(self) -> Result<QueryPlan, BuildError> {
        if self.steps.is_empty() {
            return Err(BuildError::EmptyBuilder);
        }

        let first = &self.steps[0];
        if !first.is_context_free() {
            return Err(BuildError::FirstStepNotContextFree {
                first_kind: PlanNodeKind::of(first),
            });
        }

        for step in &self.steps {
            validate_subtree(step)?;
        }

        Ok(QueryPlan::new(PlanNode::Chain { steps: self.steps }))
    }
}

/// Inlines another plan's root into a builder's `steps` vector.
///
/// Used by [`QueryBuilder::combine`] when the builder is empty. If the
/// adopted root is itself a [`PlanNode::Chain`], its steps are unwrapped so
/// that the builder doesn't end up with a `Chain { steps: vec![Chain{...}] }`
/// shape (which would fail context-free validation at `build` time and is
/// also semantically redundant). Any other variant is appended as a single
/// step.
fn adopt_into_steps(steps: &mut Vec<PlanNode>, root: PlanNode) {
    match root {
        PlanNode::Chain { steps: inner } => steps.extend(inner),
        other => steps.push(other),
    }
}

/// Recursive structural validation shared by [`QueryBuilder::build`] and the
/// set-op operand check.
///
/// Checks every traversal in the subtree for `max_depth >= 1` and verifies
/// that every nested [`PlanNode::SetOp`] operand is itself context-free.
/// Filter predicates carrying [`PredicateValue::Subquery`] are also
/// validated, so a malformed subquery surfaces at top-level `build` time.
fn validate_subtree(node: &PlanNode) -> Result<(), BuildError> {
    match node {
        PlanNode::NodeScan { .. } => Ok(()),
        PlanNode::EdgeTraversal { max_depth, .. } => {
            if *max_depth == 0 {
                Err(BuildError::ZeroDepth)
            } else {
                Ok(())
            }
        }
        PlanNode::Filter { predicate } => validate_predicate(predicate),
        PlanNode::SetOp { left, right, .. } => {
            ensure_context_free(left)?;
            ensure_context_free(right)?;
            validate_subtree(left)?;
            validate_subtree(right)?;
            Ok(())
        }
        PlanNode::Chain { steps } => {
            if let Some(first) = steps.first()
                && !first.is_context_free()
            {
                return Err(BuildError::FirstStepNotContextFree {
                    first_kind: PlanNodeKind::of(first),
                });
            }
            for step in steps {
                validate_subtree(step)?;
            }
            Ok(())
        }
    }
}

/// Recursively validates a [`Predicate`] tree, walking through every boolean
/// combinator and into nested subqueries.
fn validate_predicate(predicate: &Predicate) -> Result<(), BuildError> {
    match predicate {
        Predicate::HasCaller
        | Predicate::HasCallee
        | Predicate::IsUnused
        | Predicate::InFile(_)
        | Predicate::InScope(_)
        | Predicate::MatchesName(_) => Ok(()),

        Predicate::Callers(v)
        | Predicate::Callees(v)
        | Predicate::Imports(v)
        | Predicate::Exports(v)
        | Predicate::References(v)
        | Predicate::Implements(v) => validate_predicate_value(v),

        Predicate::And(list) | Predicate::Or(list) => {
            for inner in list {
                validate_predicate(inner)?;
            }
            Ok(())
        }
        Predicate::Not(inner) => validate_predicate(inner),
    }
}

/// Validates the right-hand side of a relation predicate. For subqueries the
/// nested plan must itself be a well-formed [`PlanNode`].
fn validate_predicate_value(value: &PredicateValue) -> Result<(), BuildError> {
    match value {
        PredicateValue::Pattern(_) | PredicateValue::Regex(_) => Ok(()),
        PredicateValue::Subquery(plan) => {
            ensure_context_free(plan)?;
            validate_subtree(plan)
        }
    }
}

/// Helper used by set-op operand and subquery validation. Wraps the
/// underlying context-free check so the resulting error variant carries the
/// `InvalidSetOpOperand` framing.
fn ensure_context_free(node: &PlanNode) -> Result<(), BuildError> {
    if node.is_context_free() {
        return Ok(());
    }

    // A `Chain` whose first step is itself context-free is acceptable as a
    // sub-plan operand — the chain's contract is the same as a top-level
    // builder's.
    if let PlanNode::Chain { steps } = node
        && let Some(first) = steps.first()
        && first.is_context_free()
    {
        return Ok(());
    }

    Err(BuildError::InvalidSetOpOperand {
        reason: format!("operand root is {:?}", PlanNodeKind::of(node)),
    })
}

// ============================================================================
// QueryPlan ergonomics — used to compose builder outputs as subqueries
// ============================================================================

/// Extension methods on [`QueryPlan`] that simplify common composition
/// patterns. Implemented as a trait so the [`ir`] module stays free of
/// `compile`-flavoured concerns.
pub trait QueryPlanExt {
    /// Wraps the plan's root as a [`PredicateValue::Subquery`] suitable for
    /// embedding inside a relation predicate (e.g. `Predicate::Callers`).
    ///
    /// Consumes the plan to avoid a clone — callers needing both the plan
    /// and the subquery should `.clone()` first.
    fn into_subquery(self) -> PredicateValue;

    /// Borrowing variant of [`Self::into_subquery`] that clones the plan's
    /// root rather than consuming it.
    fn as_subquery(&self) -> PredicateValue;
}

impl QueryPlanExt for QueryPlan {
    fn into_subquery(self) -> PredicateValue {
        PredicateValue::Subquery(Box::new(self.root))
    }

    fn as_subquery(&self) -> PredicateValue {
        PredicateValue::Subquery(Box::new(self.root.clone()))
    }
}

// ============================================================================
// Inline unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_builder_reports_empty_error() {
        let err = QueryBuilder::new().build().unwrap_err();
        assert_eq!(err, BuildError::EmptyBuilder);
    }

    #[test]
    fn first_step_filter_is_rejected() {
        let mut b = QueryBuilder::new();
        b.steps.push(PlanNode::Filter {
            predicate: Predicate::HasCaller,
        });
        let err = b.build().unwrap_err();
        assert!(matches!(
            err,
            BuildError::FirstStepNotContextFree {
                first_kind: PlanNodeKind::Filter
            }
        ));
    }

    #[test]
    fn first_step_traversal_is_rejected() {
        let mut b = QueryBuilder::new();
        b.steps.push(PlanNode::EdgeTraversal {
            direction: Direction::Forward,
            edge_kind: None,
            max_depth: 1,
        });
        let err = b.build().unwrap_err();
        assert!(matches!(
            err,
            BuildError::FirstStepNotContextFree {
                first_kind: PlanNodeKind::EdgeTraversal
            }
        ));
    }

    #[test]
    fn zero_depth_is_rejected() {
        let err = QueryBuilder::new()
            .scan(NodeKind::Function)
            .traverse_any(Direction::Forward, 0)
            .build()
            .unwrap_err();
        assert_eq!(err, BuildError::ZeroDepth);
    }

    #[test]
    fn scan_then_filter_builds() {
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .filter(Predicate::HasCaller)
            .build()
            .expect("plan");
        let PlanNode::Chain { steps } = &plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], PlanNode::NodeScan { .. }));
        assert!(matches!(steps[1], PlanNode::Filter { .. }));
    }

    #[test]
    fn step_count_and_is_empty_track_state() {
        let b = QueryBuilder::new();
        assert!(b.is_empty());
        assert_eq!(b.step_count(), 0);

        let b = b.scan(NodeKind::Function).filter(Predicate::HasCaller);
        assert!(!b.is_empty());
        assert_eq!(b.step_count(), 2);
    }

    #[test]
    fn normalize_edge_kind_zeroes_calls_metadata() {
        let a = normalize_edge_kind(EdgeKind::Calls {
            argument_count: 7,
            is_async: true,
        });
        let b = normalize_edge_kind(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        });
        assert_eq!(a, b);
    }

    #[test]
    fn normalize_edge_kind_passes_through_metadata_free_variants() {
        let cases = [
            EdgeKind::Defines,
            EdgeKind::Contains,
            EdgeKind::References,
            EdgeKind::Inherits,
            EdgeKind::Implements,
            EdgeKind::WebAssemblyCall,
        ];
        for c in cases {
            assert_eq!(normalize_edge_kind(c.clone()), c);
        }
    }

    #[test]
    fn plan_node_kind_of_covers_all_variants() {
        assert_eq!(
            PlanNodeKind::of(&PlanNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: None
            }),
            PlanNodeKind::NodeScan
        );
        assert_eq!(
            PlanNodeKind::of(&PlanNode::EdgeTraversal {
                direction: Direction::Forward,
                edge_kind: None,
                max_depth: 1
            }),
            PlanNodeKind::EdgeTraversal
        );
        assert_eq!(
            PlanNodeKind::of(&PlanNode::Filter {
                predicate: Predicate::HasCaller
            }),
            PlanNodeKind::Filter
        );
        let scan = PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        };
        assert_eq!(
            PlanNodeKind::of(&PlanNode::SetOp {
                op: super::super::ir::SetOperation::Union,
                left: Box::new(scan.clone()),
                right: Box::new(scan)
            }),
            PlanNodeKind::SetOp
        );
        assert_eq!(
            PlanNodeKind::of(&PlanNode::Chain { steps: vec![] }),
            PlanNodeKind::Chain
        );
    }

    #[test]
    fn into_subquery_consumes_plan() {
        let plan = QueryBuilder::new()
            .scan(NodeKind::Function)
            .build()
            .expect("plan");
        let value = plan.into_subquery();
        assert!(value.is_subquery());
    }
}
