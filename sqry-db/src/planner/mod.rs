//! Structural query planner for `sqry-db`.
//!
//! The planner translates user queries (text syntax or builder API calls) into
//! a [`QueryPlan`] — a tree of [`PlanNode`] operators — that the executor walks
//! to produce result sets by dispatching to [`DerivedQuery`] implementations in
//! the query cache layer.
//!
//! # Pipeline Stages
//!
//! ```text
//! text syntax / builder API
//!         │
//!         ▼
//!     [ir]  ─── snapshot-independent plan tree
//!         │
//!         ▼
//!   [compile]  ── QueryBuilder materialises `PlanNode` tree (DB10)
//!         │
//!         ▼
//!   [fuse]     ── merge shared NodeScan prefixes across plans (DB11)
//!         │
//!         ▼
//!   [execute]  ── dispatch PlanNode variants to DerivedQuery calls (DB12)
//!         │
//!         ▼
//!     Vec<NodeId>
//! ```
//!
//! # Module status
//!
//! - **DB09 (IR)** ✅ landed in [`ir`].
//! - **DB10 (builder)** ✅ landed in [`compile`]. Fluent builder API plus
//!   [`compile::normalize_edge_kind`] which canonicalises site-level edge
//!   metadata while preserving semantic discriminators (see the helper's
//!   docstring for the per-variant policy).
//! - **DB11 (fuser)** ✅ landed in [`fuse`]. Merges shared `NodeScan`
//!   prefixes across batches of plans, including recursive subquery
//!   deduplication.
//! - **DB12 (executor)** ✅ landed in [`execute`]. Walks a [`QueryPlan`]
//!   tree and produces a sorted, deduplicated `Vec<NodeId>`. Accepts either
//!   a single plan or a [`fuse::FusedPlanBatch`]; shared prefixes and
//!   duplicated subqueries evaluate exactly once per call.
//!
//! # Design References
//!
//! - Spec: `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md` (§3)
//! - DAG: `docs/superpowers/plans/2026-04-12-phase3-4-combined-implementation-dag.toml` (units DB09-DB13)
//!
//! [`DerivedQuery`]: crate::query::DerivedQuery

pub mod compile;
pub mod cost_gate;
pub mod execute;
pub mod fuse;
pub mod ir;
pub mod parse;

pub use compile::{
    BuildError, PlanNodeKind, QueryBuilder, QueryPlanExt, ScanFilters, normalize_edge_kind,
};
pub use execute::{PlanExecutor, execute_batch, execute_plan};
pub use fuse::{
    FusedPlanBatch, FusedTail, FusionGroup, FusionStats, FusionTail, SharedNode, SharedNodeId,
    fuse_plans, fuse_single,
};
pub use ir::{
    Direction, MatchMode, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan, RegexFlags,
    RegexPattern, SetOperation, StringPattern,
};
pub use parse::{ParseError, parse_query};
