//! Query plan intermediate representation.
//!
//! The IR is **snapshot-independent**: no `StringId`, `NodeId`, or other
//! interner-bound handles appear anywhere. This means a single plan can be
//! compiled once and evaluated against any compatible graph snapshot, and two
//! plans constructed independently but with identical semantics hash to the
//! same value — a property the fuser in [`super::fuse`] relies on.
//!
//! # Types in this module
//!
//! | Type | Role |
//! |------|------|
//! | [`QueryPlan`] | Top-level wrapper produced by [`super::compile::QueryBuilder`] (DB10). |
//! | [`PlanNode`] | Recursive operator tree. Every node produces a node-set. |
//! | [`Direction`] | Edge traversal direction (outgoing/incoming/both). |
//! | [`SetOperation`] | Pairwise set algebra (union/intersect/difference). |
//! | [`Predicate`] | Filter condition — existence checks, relation predicates, boolean combinators. |
//! | [`PredicateValue`] | Right-hand side of a relation predicate: pattern, regex, or nested subquery. |
//! | [`StringPattern`] | Name-matching pattern with [`MatchMode`] discriminator. |
//! | [`PathPattern`] | File-path glob matching, always glob-based. |
//! | [`RegexPattern`] | Regex source + compilation flags. |
//! | [`MatchMode`] | Exact / Glob / Prefix / Suffix / Contains modes for [`StringPattern`]. |
//!
//! # Design Notes
//!
//! ## Edge kind filtering
//!
//! [`PlanNode::EdgeTraversal`] carries `edge_kind: Option<EdgeKind>`, matching
//! the spec (§3). Because [`EdgeKind`] contains metadata fields that are
//! irrelevant to traversal (e.g., `Calls { argument_count, is_async }`), the
//! [`QueryBuilder`] is expected to construct [`EdgeKind`] values with zeroed
//! metadata (e.g., `EdgeKind::Calls { argument_count: 0, is_async: false }`)
//! so that plan hashing remains stable for the fuser. The executor matches
//! by `std::mem::discriminant`, matching the pattern used by
//! [`crate::queries::ReachabilityQuery`].
//!
//! ## Scope filtering
//!
//! [`Predicate::InScope`] re-uses [`ScopeKind`] from the binding plane
//! (`sqry-core::graph::unified::bind::scope::arena::ScopeKind`). The existing
//! enum is already `Copy`, `Hash`, `Eq`, `Serialize`, `Deserialize`, so no
//! wrapper type is introduced.
//!
//! ## Subqueries
//!
//! [`PredicateValue::Subquery`] is boxed to break the `Predicate`→`PlanNode`
//! recursion cycle. Subqueries are evaluated on-demand by the executor,
//! and their results are cached through [`crate::QueryDb::get`].
//!
//! ## Serialization
//!
//! Every IR type derives `Serialize` and `Deserialize`. This is required so
//! that plans can be fingerprinted for the fuser, embedded in structured log
//! output, and (in DB22) persisted in `.sqry/graph/derived.sqry` as part of
//! hot-cache keys.
//!
//! [`EdgeKind`]: sqry_core::graph::unified::edge::kind::EdgeKind
//! [`ScopeKind`]: sqry_core::graph::unified::bind::scope::arena::ScopeKind
//! [`QueryBuilder`]: super::compile::QueryBuilder

use serde::{Deserialize, Serialize};

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::schema::{FrameworkId, Visibility};

/// Top-level query plan produced by the compiler.
///
/// A plan is a rooted tree of [`PlanNode`] operators. The executor walks the
/// tree from the root, evaluating each node's output into a node-set and
/// feeding it forward through [`PlanNode::Chain`] and [`PlanNode::SetOp`]
/// combinators.
///
/// The thin wrapper exists so that:
///
/// - the fuser in [`super::fuse`] can operate on a list of `QueryPlan`s
///   without confusion about which `PlanNode` is the root,
/// - plan-level metadata can be added later (e.g., execution hints, limits)
///   without touching every [`PlanNode`] variant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueryPlan {
    /// The root operator of the plan tree.
    pub root: PlanNode,
}

impl QueryPlan {
    /// Creates a new plan wrapping the given root operator.
    #[must_use]
    pub fn new(root: PlanNode) -> Self {
        Self { root }
    }

    /// Returns a reference to the plan's root operator.
    #[inline]
    #[must_use]
    pub fn root(&self) -> &PlanNode {
        &self.root
    }

    /// Returns the total number of [`PlanNode`] operators in the tree,
    /// counting every node exactly once.
    ///
    /// Useful for plan-complexity metrics, fusion heuristics, and tests.
    #[must_use]
    pub fn operator_count(&self) -> usize {
        self.root.operator_count()
    }
}

/// Operator tree for a structural query.
///
/// Every variant produces a node-set. Consumers treat the resulting
/// `Vec<NodeId>` as a sorted, deduplicated sequence.
///
/// # Variant semantics
///
/// - [`PlanNode::NodeScan`] scans the graph and emits every node matching the
///   supplied filters. This is the only variant without an input set.
/// - [`PlanNode::EdgeTraversal`] requires an input set (supplied by the
///   enclosing [`PlanNode::Chain`]) and follows edges from those nodes.
/// - [`PlanNode::Filter`] narrows its input set by evaluating the predicate.
/// - [`PlanNode::SetOp`] evaluates two sub-plans independently and combines
///   the results. Neither side inherits an input set from an enclosing chain.
/// - [`PlanNode::Chain`] threads the output of each step into the next step's
///   input. The first step must be context-free (a `NodeScan` or `SetOp`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanNode {
    /// Scan all nodes matching the given filters.
    ///
    /// Each filter is an optional narrowing condition; `None` means
    /// "accept any value". `kind = None`, `visibility = None`,
    /// `name_pattern = None` yields a full node scan.
    NodeScan {
        /// Optional kind filter (Function, Method, Class, ...).
        kind: Option<NodeKind>,
        /// Optional visibility filter (Public, Private).
        visibility: Option<Visibility>,
        /// Optional symbol-name pattern filter.
        name_pattern: Option<StringPattern>,
    },
    /// Follow edges outward from the input set.
    ///
    /// `edge_kind = None` matches any edge kind. `max_depth = 1` performs a
    /// single hop; values greater than one perform a bounded BFS.
    EdgeTraversal {
        /// Direction in which to follow edges.
        direction: Direction,
        /// Optional edge-kind filter. See module docs for how the executor
        /// compares variants by discriminant.
        edge_kind: Option<EdgeKind>,
        /// Maximum hops to follow. Must be `>= 1` to produce any output.
        max_depth: u32,
        /// Optional resolution-provenance filter applied AFTER the
        /// discriminant filter (DESIGN §6.3bis Mechanism B). When `Some`,
        /// the traversal additionally requires every traversed
        /// [`EdgeKind::Calls`] edge to carry the matching
        /// [`ResolvedVia`] tag; non-`Calls` edges are unaffected so this
        /// field is meaningful only when paired with a `Calls`
        /// `edge_kind`. `None` (the default for every legacy
        /// construction) preserves pre-U15 behavior.
        resolved_via: Option<ResolvedVia>,
    },
    /// Narrow the input set with a predicate.
    Filter {
        /// The predicate to evaluate for each input node.
        predicate: Predicate,
    },
    /// Combine two independently-evaluated sub-plans with a set operation.
    SetOp {
        /// The set operation: Union, Intersect, or Difference.
        op: SetOperation,
        /// Left-hand operand sub-plan.
        left: Box<PlanNode>,
        /// Right-hand operand sub-plan.
        right: Box<PlanNode>,
    },
    /// Pipeline a sequence of steps, threading each step's output into the
    /// next step's input.
    ///
    /// The first step must be context-free (a [`PlanNode::NodeScan`] or
    /// [`PlanNode::SetOp`]); subsequent steps are applied to the running set.
    Chain {
        /// Steps to evaluate in order. An empty vec yields the empty set.
        steps: Vec<PlanNode>,
    },
}

impl PlanNode {
    /// Returns the total number of operators in this subtree.
    ///
    /// Counts this node plus all descendants through
    /// [`PlanNode::SetOp::left`], [`PlanNode::SetOp::right`], and
    /// [`PlanNode::Chain::steps`]. [`PlanNode::Filter`]'s predicate tree
    /// is *not* counted here — predicate trees have their own complexity.
    #[must_use]
    pub fn operator_count(&self) -> usize {
        match self {
            PlanNode::NodeScan { .. }
            | PlanNode::EdgeTraversal { .. }
            | PlanNode::Filter { .. } => 1,
            PlanNode::SetOp { left, right, .. } => {
                1 + left.operator_count() + right.operator_count()
            }
            PlanNode::Chain { steps } => {
                1 + steps.iter().map(PlanNode::operator_count).sum::<usize>()
            }
        }
    }

    /// Returns `true` if this node has no input dependency (can be evaluated
    /// in isolation).
    ///
    /// Used by the compiler to validate [`PlanNode::Chain::steps`] — the
    /// first step must be context-free.
    #[must_use]
    pub fn is_context_free(&self) -> bool {
        matches!(self, PlanNode::NodeScan { .. } | PlanNode::SetOp { .. })
    }
}

/// Direction of an edge traversal relative to the input node set.
///
/// # Semantics
///
/// - [`Direction::Forward`] — follow outgoing edges (`source -> target`).
///   `callees:X` is a forward traversal of `Calls` edges from `X`.
/// - [`Direction::Reverse`] — follow incoming edges (`target -> source`).
///   `callers:X` is a reverse traversal of `Calls` edges into `X`.
/// - [`Direction::Both`] — follow edges in both directions. Useful for
///   symmetric relations and for impact analysis that should include both
///   callers and callees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow outgoing edges (`source -> target`).
    Forward,
    /// Follow incoming edges (`target -> source`).
    Reverse,
    /// Follow edges in both directions.
    Both,
}

impl Direction {
    /// Returns the inverse direction.
    ///
    /// `Forward <-> Reverse`, `Both` is its own inverse.
    #[must_use]
    pub const fn invert(self) -> Self {
        match self {
            Direction::Forward => Direction::Reverse,
            Direction::Reverse => Direction::Forward,
            Direction::Both => Direction::Both,
        }
    }
}

/// Set algebra on two node-set results.
///
/// All three operators are commutative in the output set, but order is
/// preserved in the IR so that the executor can stream the smaller side
/// first in the fuser pass (DB11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetOperation {
    /// Union: `left ∪ right`.
    Union,
    /// Intersection: `left ∩ right`.
    Intersect,
    /// Difference: `left \ right` (elements in left but not right).
    Difference,
}

/// Filter condition applied to a node-set.
///
/// Predicates fall into four groups:
///
/// 1. **Existence checks**: [`Predicate::HasCaller`], [`Predicate::HasCallee`],
///    [`Predicate::IsUnused`]. Cheap — no value or subquery traversal needed.
/// 2. **Value-bearing relation predicates**: [`Predicate::Callers`] through
///    [`Predicate::Implements`]. These accept a [`PredicateValue`] on the
///    right-hand side — a literal pattern, a regex, or a nested subquery.
///    They map 1:1 to the six relation handlers in
///    `sqry-core::query::executor::graph_eval` (`match_callers`,
///    `match_callees`, `match_imports`, `match_exports`, `match_references`,
///    `match_implements`) and their `_subquery` variants.
/// 3. **Attribute filters**: [`Predicate::InFile`], [`Predicate::InScope`],
///    [`Predicate::MatchesName`].
/// 4. **Boolean combinators**: [`Predicate::And`], [`Predicate::Or`],
///    [`Predicate::Not`].
///
/// # Semantic alignment with `graph_eval`
///
/// The six relation variants exactly mirror the value-bearing operators in
/// `sqry-core::query::types::Value`:
///
/// | This IR | `graph_eval` handler | Text syntax |
/// |---------|----------------------|-------------|
/// | `Callers(v)`     | `match_callers` / `match_callers_subquery`       | `callers:v`    |
/// | `Callees(v)`     | `match_callees` / `match_callees_subquery`       | `callees:v`    |
/// | `Imports(v)`     | `match_imports` / `match_imports_subquery`       | `imports:v`    |
/// | `Exports(v)`     | `match_exports` / `match_exports_subquery`       | `exports:v`    |
/// | `References(v)`  | `match_references` / `match_references_subquery` | `references:v` (supports `~=` regex) |
/// | `Implements(v)`  | `match_implements` / `match_implements_subquery` | `impl:v` / `implements:v` |
///
/// The text frontend in DB13 must accept both `impl:` and `implements:`
/// aliases for [`Predicate::Implements`] (spec §M8).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    // --- Existence checks ---
    /// True iff the node has at least one incoming `Calls` edge.
    HasCaller,
    /// True iff the node has at least one outgoing `Calls` edge.
    HasCallee,
    /// True iff the node is not reachable from any entry point.
    IsUnused,

    // --- C indirect-call precision (Phase A) ---
    //
    // Surfaces the three Phase-A flags + resolution-provenance variants
    // described in DESIGN §11.1. The spellings are user-visible and
    // **locked** per DESIGN §11.1's predicate-spelling table:
    //
    //   * `address_taken:true|false` (or bare `address_taken` => true)
    //   * `resolved_via:direct | type_match | binding_plane`
    //   * `callsite_promiscuous:true|false` (bare => true)
    //
    /// Filters to nodes whose [`NodeFlags::ADDRESS_TAKEN`] bit matches the
    /// requested polarity (true = flag set; false = flag clear).
    ///
    /// Backed by the metadata-store accessor
    /// `GraphSnapshot::macro_metadata().is_address_taken(node_id)`; the
    /// cached set is also available through
    /// [`crate::queries::AddressTakenQuery`] (Tier-2 + Tier-3) for
    /// callers preferring a derived-query handle.
    ///
    /// [`NodeFlags::ADDRESS_TAKEN`]: sqry_core::graph::unified::storage::NodeFlags::ADDRESS_TAKEN
    IsAddressTaken(bool),
    /// Filters to nodes which are the source of at least one outgoing
    /// `EdgeKind::Calls` edge with the requested
    /// [`ResolvedVia`] resolution provenance.
    ///
    /// Two cooperating mechanisms back this predicate (DESIGN §6.3bis):
    /// non-adjacent occurrences land as this `Filter` step and probe
    /// outgoing `Calls` edges; occurrences adjacent to a `Calls`
    /// traversal fold into [`PlanNode::EdgeTraversal`]'s `resolved_via`
    /// discriminator (U15 — not yet landed at U14).
    ResolvedVia(ResolvedVia),
    /// Filters to nodes whose [`NodeFlags::CALLSITE_PROMISCUOUS`] marker
    /// matches the requested polarity. Mirrors [`Self::IsAddressTaken`]
    /// — flag-only lookup against
    /// `GraphSnapshot::macro_metadata().is_callsite_promiscuous`.
    ///
    /// [`NodeFlags::CALLSITE_PROMISCUOUS`]: sqry_core::graph::unified::storage::NodeFlags::CALLSITE_PROMISCUOUS
    HasCallsitePromiscuous(bool),

    // --- Phase β joint-stubs (Plan A + Plan B coordination) ---
    //
    // Both predicates ship in the joint-stubs PR. They parse, compile,
    // fuse, cost, and evaluate against the empty side-table surface
    // exposed by `NodeMetadataStore::framework_routes` /
    // `dispatch_tables`. Plan A / Plan B's downstream PRs will replace
    // the no-op evaluation with full lookups once the extractors /
    // resolvers populate the underlying tables.
    /// Plan A (framework-route extractors): filters to nodes whose
    /// `NodeMetadataStore::framework_route` entry was tagged by the named
    /// framework. In the joint-stubs PR no node carries framework
    /// metadata, so this predicate evaluates to `false` for every
    /// candidate. The variant exists so the planner / parser / MCP
    /// filter param can be wired end-to-end and the downstream PR only
    /// has to swap the executor body.
    ///
    /// Text frontend: `framework:<id>` (e.g. `framework:flask`).
    FrameworkEq(FrameworkId),
    /// Plan B (dispatch-resolver work): filters to nodes whose outgoing
    /// `Calls` edges include at least one resolution provenance in the
    /// requested set. Set-membership form to mirror the MCP
    /// `resolved_via: Vec<ResolvedVia>` filter param. The existing
    /// single-value [`Self::ResolvedVia`] predicate stays as the
    /// canonical text-frontend lowering for one-of-a-kind probes; this
    /// new variant is reached via the MCP filter path (or a future
    /// planner-grammar extension that accepts a comma-separated list).
    ///
    /// In the joint-stubs PR the executor uses the same outgoing-Calls
    /// walk as [`Self::ResolvedVia`] but accepts every requested variant
    /// in the set. Today no `Calls` edge carries any of the new
    /// resolution-provenance variants Plan B will add (VirtualDispatch,
    /// InterfaceDispatch, DuckTyped, Structural, PromiscuousElided),
    /// so the predicate degenerates to the existing 3-variant union
    /// over `Direct`, `TypeMatch`, `BindingPlane` — exactly the same
    /// behaviour as the existing `ResolvedVia` predicate when those
    /// variants are passed in.
    ResolvedViaEq(Vec<ResolvedVia>),

    // --- Value-bearing relation predicates ---
    /// `callers:<value>`: node's callers match the pattern / subquery.
    Callers(PredicateValue),
    /// `callees:<value>`: node's callees match the pattern / subquery.
    Callees(PredicateValue),
    /// `imports:<value>`: node's imports match the pattern / subquery.
    Imports(PredicateValue),
    /// `exports:<value>`: node's exports match the pattern / subquery.
    Exports(PredicateValue),
    /// `references:<value>` (also supports `~=` regex via
    /// [`PredicateValue::Regex`]).
    References(PredicateValue),
    /// `impl:<value>` / `implements:<value>`: matches nodes implementing
    /// the referenced trait / interface.
    Implements(PredicateValue),

    // --- Attribute filters ---
    /// `in:<path-glob>`: true iff the node's file path matches the glob.
    InFile(PathPattern),
    /// `scope:<kind>`: true iff the node's enclosing scope kind matches.
    InScope(ScopeKind),
    /// `name:<pattern>`: true iff the node's name matches the pattern.
    MatchesName(StringPattern),
    /// `returns:<TypeName>`: true iff the node (a function or method) has at
    /// least one outgoing
    /// [`EdgeKind::TypeOf { context: Some(TypeOfContext::Return), .. }`][crate::queries]
    /// edge whose target node's interned name matches `TypeName` exactly.
    ///
    /// # Semantics
    ///
    /// The match is evaluated **edge-based, not signature-text-based**: the
    /// executor walks `TypeOf` edges with `TypeOfContext::Return` from the
    /// candidate node, resolves the target node's primary name through the
    /// snapshot string interner, and compares with byte-exact equality
    /// (case-sensitive). Substring, glob, and regex variants are deliberately
    /// out of scope for this predicate; a future `returns~:` token (regex
    /// form) will lower to a separate IR variant rather than overload this
    /// one — the same shape used today for [`Predicate::References`] vs the
    /// `references ~= /…/` regex form.
    ///
    /// # Why a bare `String` instead of [`StringPattern`]?
    ///
    /// [`StringPattern`] auto-detects glob meta-characters and promotes to
    /// [`MatchMode::Glob`]; modelling the predicate value with `String`
    /// keeps the contract narrow — `returns:Foo*` is a parse error in
    /// users' minds, not a hidden glob expansion. The single-mode shape
    /// also keeps cache keys monomorphic which simplifies any future
    /// derived-query backing for this predicate.
    Returns(String),

    // --- T3 Cluster F: conditional-compilation + error-wrap predicates ---
    /// `cfg:<value>` — true iff the node's `MacroNodeMetadata::cfg_condition`
    /// satisfies the carried [`crate::planner::cfg_match::CfgMatcher`].
    ///
    /// Bare planner tokens (`cfg:linux`) parse to
    /// [`CfgMatcher::Semantic`] — the cross-language comparator
    /// matches both Go-stored `"linux"` and Rust-stored
    /// `"target_os = \"linux\""`. Quoted forms
    /// (`cfg:"linux && amd64"`, `cfg:"target_os = \"linux\""`)
    /// parse to [`CfgMatcher::Literal`] and stay byte-exact /
    /// language-specific per 02_DESIGN §5.3.a + §10.4 (the two
    /// addressing modes are kept independently observable so users
    /// can opt in to either).
    ///
    /// [`CfgMatcher::Semantic`]: crate::planner::cfg_match::CfgMatcher::Semantic
    /// [`CfgMatcher::Literal`]: crate::planner::cfg_match::CfgMatcher::Literal
    CfgCondition(super::cfg_match::CfgMatcher),

    /// `wraps` / `wraps:<wrap-kind>` — true iff the node has at least
    /// one outbound [`EdgeKind::Wraps`] edge whose `kind` field
    /// satisfies the carried [`WrapKindFilter`] (T3.6 acceptance
    /// criterion 9; 02_DESIGN §2.1).
    Wraps(WrapKindFilter),

    // --- Boolean combinators ---
    /// Logical AND over a list of predicates. Empty list is vacuously true.
    And(Vec<Predicate>),
    /// Logical OR over a list of predicates. Empty list is vacuously false.
    Or(Vec<Predicate>),
    /// Logical NOT of a single predicate.
    Not(Box<Predicate>),
}

/// Filter applied by [`Predicate::Wraps`] over outbound `Wraps` edges.
///
/// `Any` accepts every `Wraps` discriminant (the bare `wraps`
/// planner predicate); `Kind(k)` keeps only edges with the matching
/// `WrapKind`. Per 02_DESIGN §2.1 the planner relies on
/// [`super::compile::normalize_edge_kind`] preserving `WrapKind` for
/// cache-bucket distinctness, while a second-stage filter at the
/// executor enforces the actual `kind` comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapKindFilter {
    /// Bare `wraps` — accept every `WrapKind`.
    Any,
    /// `wraps:<kind>` — only accept edges with the named kind.
    Kind(sqry_core::graph::unified::edge::WrapKind),
}

impl Predicate {
    /// Returns `true` if this predicate (or any nested predicate through
    /// boolean combinators) references a [`PredicateValue::Subquery`].
    ///
    /// The executor uses this hint to decide whether to allocate subquery
    /// evaluation scratch space up front.
    #[must_use]
    pub fn has_subquery(&self) -> bool {
        match self {
            Predicate::HasCaller
            | Predicate::HasCallee
            | Predicate::IsUnused
            | Predicate::IsAddressTaken(_)
            | Predicate::ResolvedVia(_)
            | Predicate::HasCallsitePromiscuous(_)
            | Predicate::FrameworkEq(_)
            | Predicate::ResolvedViaEq(_)
            | Predicate::InFile(_)
            | Predicate::InScope(_)
            | Predicate::MatchesName(_)
            | Predicate::Returns(_)
            | Predicate::CfgCondition(_)
            | Predicate::Wraps(_) => false,

            Predicate::Callers(v)
            | Predicate::Callees(v)
            | Predicate::Imports(v)
            | Predicate::Exports(v)
            | Predicate::References(v)
            | Predicate::Implements(v) => v.is_subquery(),

            Predicate::And(list) | Predicate::Or(list) => list.iter().any(Predicate::has_subquery),
            Predicate::Not(inner) => inner.has_subquery(),
        }
    }
}

/// Right-hand side of a relation predicate.
///
/// Mirrors `sqry-core::query::types::Value`'s String / Regex / Subquery
/// arms that actually flow into the six relation handlers. The IR variant
/// is deliberately narrower than the general-purpose [`Value`] — numeric
/// and boolean values never appear on the right of a relation predicate,
/// so they are omitted here.
///
/// [`Value`]: sqry_core::query::types::Value
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PredicateValue {
    /// Literal string pattern — exact match, glob, prefix, suffix, or
    /// contains. Maps to `Operator::Equal` in `graph_eval`.
    Pattern(StringPattern),
    /// Regex pattern. Maps to `Operator::Regex` in `graph_eval`.
    ///
    /// Only [`Predicate::References`] uses this in the current text
    /// syntax (`references ~= /foo.*/i`), but any relation predicate
    /// may accept it at the IR level.
    Regex(RegexPattern),
    /// Nested subquery: `callers:(kind:function AND async:true)`.
    /// Evaluated as a distinct [`PlanNode`] before the outer relation
    /// predicate runs.
    Subquery(Box<PlanNode>),
}

impl PredicateValue {
    /// Returns `true` if this value is a [`PredicateValue::Subquery`].
    #[must_use]
    pub const fn is_subquery(&self) -> bool {
        matches!(self, PredicateValue::Subquery(_))
    }

    /// Returns the inner [`PlanNode`] if this value is a subquery.
    #[must_use]
    pub fn as_subquery(&self) -> Option<&PlanNode> {
        match self {
            PredicateValue::Subquery(plan) => Some(plan),
            _ => None,
        }
    }
}

/// Symbol-name matching pattern.
///
/// The [`MatchMode`] discriminator selects between exact, glob, prefix,
/// suffix, and contains semantics. The raw pattern is kept as a `String`
/// so the executor can compile it lazily.
///
/// # Why not `Arc<str>`?
///
/// The IR is a data description — compact and easy to clone. Patterns are
/// small (typically under 64 bytes), and cloning the IR is rare (once per
/// plan submission). The `Arc<str>` savings do not justify the added
/// complexity at this layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StringPattern {
    /// The raw pattern string.
    pub raw: String,
    /// How the pattern should be interpreted by the executor.
    pub mode: MatchMode,
    /// Whether matching is case-insensitive. Defaults to `false` (case
    /// sensitive) to match `graph_eval` semantics.
    #[serde(default)]
    pub case_insensitive: bool,
}

impl StringPattern {
    /// Creates an exact-match pattern (case-sensitive).
    #[must_use]
    pub fn exact(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            mode: MatchMode::Exact,
            case_insensitive: false,
        }
    }

    /// Creates a glob pattern (case-sensitive).
    #[must_use]
    pub fn glob(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            mode: MatchMode::Glob,
            case_insensitive: false,
        }
    }

    /// Creates a prefix-match pattern (case-sensitive).
    #[must_use]
    pub fn prefix(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            mode: MatchMode::Prefix,
            case_insensitive: false,
        }
    }

    /// Creates a suffix-match pattern (case-sensitive).
    #[must_use]
    pub fn suffix(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            mode: MatchMode::Suffix,
            case_insensitive: false,
        }
    }

    /// Creates a substring-contains pattern (case-sensitive).
    #[must_use]
    pub fn contains(raw: impl Into<String>) -> Self {
        Self {
            raw: raw.into(),
            mode: MatchMode::Contains,
            case_insensitive: false,
        }
    }

    /// Returns a case-insensitive copy of this pattern.
    #[must_use]
    pub fn case_insensitive(mut self) -> Self {
        self.case_insensitive = true;
        self
    }
}

/// Match semantics for a [`StringPattern`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Exact string equality.
    Exact,
    /// Shell-style glob matching (`*`, `?`, `[abc]`).
    Glob,
    /// Prefix match (`str.starts_with(raw)`).
    Prefix,
    /// Suffix match (`str.ends_with(raw)`).
    Suffix,
    /// Substring match (`str.contains(raw)`).
    Contains,
}

/// File-path matching pattern. Always interpreted as a shell-style glob,
/// matching `graph_eval`'s `Operator::Equal` semantics on path fields.
///
/// The raw glob is kept as a `String`; the executor compiles the glob
/// lazily using `globset` or the project's existing glob helpers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PathPattern {
    /// The raw glob string, e.g. `src/**/*.rs`.
    pub glob: String,
}

impl PathPattern {
    /// Creates a new path glob pattern.
    #[must_use]
    pub fn new(glob: impl Into<String>) -> Self {
        Self { glob: glob.into() }
    }

    /// Returns the raw glob string.
    #[inline]
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.glob
    }
}

impl<S: Into<String>> From<S> for PathPattern {
    fn from(s: S) -> Self {
        Self::new(s)
    }
}

/// Regex pattern with compilation flags.
///
/// Mirrors `sqry-core::query::types::RegexValue` so that `graph_eval`
/// `Operator::Regex` semantics round-trip losslessly through the IR.
/// Compilation is deferred to the executor — a failed regex compile is
/// reported as a query error, not a plan-construction error.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RegexPattern {
    /// The regex source string.
    pub pattern: String,
    /// Flags that influence compilation.
    #[serde(default)]
    pub flags: RegexFlags,
}

impl RegexPattern {
    /// Creates a new regex pattern with default flags.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            flags: RegexFlags::default(),
        }
    }

    /// Creates a new regex pattern with the given flags.
    #[must_use]
    pub fn with_flags(pattern: impl Into<String>, flags: RegexFlags) -> Self {
        Self {
            pattern: pattern.into(),
            flags,
        }
    }
}

/// Regex compilation flags, mirroring
/// `sqry-core::query::types::RegexFlags`.
///
/// # Serialization
///
/// All three fields default to `false`; the `#[serde(default)]` attribute
/// keeps the JSON encoding compact when the flags are at their defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RegexFlags {
    /// Case-insensitive matching (regex flag `i`).
    #[serde(default)]
    pub case_insensitive: bool,
    /// Multiline mode: `^` and `$` match line boundaries (regex flag `m`).
    #[serde(default)]
    pub multiline: bool,
    /// Dot matches newlines (regex flag `s`).
    #[serde(default)]
    pub dot_all: bool,
}

// ============================================================================
// Inline unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_plan_wraps_root() {
        let root = PlanNode::NodeScan {
            kind: Some(NodeKind::Function),
            visibility: None,
            name_pattern: None,
        };
        let plan = QueryPlan::new(root.clone());
        assert_eq!(plan.root(), &root);
    }

    #[test]
    fn operator_count_single_node() {
        let scan = PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        };
        assert_eq!(scan.operator_count(), 1);
    }

    #[test]
    fn operator_count_nested_setop_and_chain() {
        let scan = PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        };
        let traverse = PlanNode::EdgeTraversal {
            direction: Direction::Forward,
            edge_kind: None,
            max_depth: 1,
            resolved_via: None,
        };
        let set = PlanNode::SetOp {
            op: SetOperation::Union,
            left: Box::new(scan.clone()),
            right: Box::new(scan.clone()),
        };
        let chain = PlanNode::Chain {
            steps: vec![set.clone(), traverse],
        };
        // chain(1) + setop(1) + scan(1) + scan(1) + traverse(1) = 5
        assert_eq!(chain.operator_count(), 5);
        assert_eq!(set.operator_count(), 3);
    }

    #[test]
    fn context_free_variants() {
        let scan = PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        };
        let setop = PlanNode::SetOp {
            op: SetOperation::Intersect,
            left: Box::new(scan.clone()),
            right: Box::new(scan.clone()),
        };
        let traverse = PlanNode::EdgeTraversal {
            direction: Direction::Reverse,
            edge_kind: None,
            max_depth: 1,
            resolved_via: None,
        };
        let filter = PlanNode::Filter {
            predicate: Predicate::HasCaller,
        };

        assert!(scan.is_context_free());
        assert!(setop.is_context_free());
        assert!(!traverse.is_context_free());
        assert!(!filter.is_context_free());
    }

    #[test]
    fn direction_invert_is_involution() {
        assert_eq!(Direction::Forward.invert(), Direction::Reverse);
        assert_eq!(Direction::Reverse.invert(), Direction::Forward);
        assert_eq!(Direction::Both.invert(), Direction::Both);
        for d in [Direction::Forward, Direction::Reverse, Direction::Both] {
            assert_eq!(d.invert().invert(), d);
        }
    }

    /// Phase β joint-stubs — both new variants are subquery-free leaf
    /// predicates. The `has_subquery` accessor must agree so the
    /// executor's subquery-scratch allocation does not over-provision
    /// (or, conversely, miss a planned subquery elsewhere in the
    /// surrounding plan).
    #[test]
    fn phase_beta_predicate_variants_have_no_subquery() {
        let framework = Predicate::FrameworkEq(sqry_core::schema::FrameworkId::Flask);
        assert!(!framework.has_subquery());

        let resolved_via_eq = Predicate::ResolvedViaEq(vec![
            ResolvedVia::Direct,
            ResolvedVia::TypeMatch,
            ResolvedVia::BindingPlane,
        ]);
        assert!(!resolved_via_eq.has_subquery());

        // Nested under a boolean combinator still reports no subquery.
        let nested = Predicate::And(vec![framework.clone(), resolved_via_eq.clone()]);
        assert!(!nested.has_subquery());

        // But a sibling subquery still propagates `true`.
        let with_sub = Predicate::And(vec![
            framework,
            Predicate::Callers(PredicateValue::Subquery(Box::new(PlanNode::NodeScan {
                kind: None,
                visibility: None,
                name_pattern: None,
            }))),
        ]);
        assert!(with_sub.has_subquery());
    }

    #[test]
    fn predicate_has_subquery_detects_nested_calls() {
        let leaf = Predicate::HasCaller;
        assert!(!leaf.has_subquery());

        let attr = Predicate::InFile(PathPattern::new("src/api/**"));
        assert!(!attr.has_subquery());

        let sub = Predicate::Callers(PredicateValue::Subquery(Box::new(PlanNode::NodeScan {
            kind: Some(NodeKind::Method),
            visibility: None,
            name_pattern: None,
        })));
        assert!(sub.has_subquery());

        let pattern = Predicate::Callers(PredicateValue::Pattern(StringPattern::exact("foo")));
        assert!(!pattern.has_subquery());

        let nested_in_and = Predicate::And(vec![leaf.clone(), sub.clone()]);
        assert!(nested_in_and.has_subquery());

        let nested_in_not = Predicate::Not(Box::new(sub));
        assert!(nested_in_not.has_subquery());

        let and_no_sub = Predicate::And(vec![leaf.clone(), attr.clone()]);
        assert!(!and_no_sub.has_subquery());
    }

    #[test]
    fn predicate_value_is_subquery() {
        let plan = PlanNode::NodeScan {
            kind: None,
            visibility: None,
            name_pattern: None,
        };
        let sub = PredicateValue::Subquery(Box::new(plan.clone()));
        let pat = PredicateValue::Pattern(StringPattern::exact("foo"));
        let re = PredicateValue::Regex(RegexPattern::new("^foo$"));

        assert!(sub.is_subquery());
        assert!(!pat.is_subquery());
        assert!(!re.is_subquery());

        assert_eq!(sub.as_subquery(), Some(&plan));
        assert_eq!(pat.as_subquery(), None);
        assert_eq!(re.as_subquery(), None);
    }

    #[test]
    fn string_pattern_builders_preserve_raw() {
        let raw = "parse_*";
        assert_eq!(StringPattern::exact(raw).raw, raw);
        assert_eq!(StringPattern::glob(raw).mode, MatchMode::Glob);
        assert_eq!(StringPattern::prefix(raw).mode, MatchMode::Prefix);
        assert_eq!(StringPattern::suffix(raw).mode, MatchMode::Suffix);
        assert_eq!(StringPattern::contains(raw).mode, MatchMode::Contains);
    }

    #[test]
    fn string_pattern_case_insensitive_toggle() {
        let p = StringPattern::exact("Foo").case_insensitive();
        assert!(p.case_insensitive);
    }

    #[test]
    fn path_pattern_from_str_and_as_str() {
        let p: PathPattern = "src/**/*.rs".into();
        assert_eq!(p.as_str(), "src/**/*.rs");

        let p2 = PathPattern::new(String::from("docs/**"));
        assert_eq!(p2.as_str(), "docs/**");
    }

    #[test]
    fn regex_pattern_default_flags_are_false() {
        let r = RegexPattern::new("^foo$");
        assert_eq!(r.pattern, "^foo$");
        assert!(!r.flags.case_insensitive);
        assert!(!r.flags.multiline);
        assert!(!r.flags.dot_all);
    }

    #[test]
    fn regex_pattern_with_flags() {
        let flags = RegexFlags {
            case_insensitive: true,
            multiline: true,
            dot_all: false,
        };
        let r = RegexPattern::with_flags("foo", flags);
        assert!(r.flags.case_insensitive);
        assert!(r.flags.multiline);
        assert!(!r.flags.dot_all);
    }
}
