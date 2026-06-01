//! Derived queries: SCC, condensation, reachability, plus relation and
//! analysis queries migrated from `sqry-core::query::executor::graph_eval`.
//!
//! # Foundational queries
//!
//! - [`SccQuery`]: Tarjan SCC computation, cached by edge kind
//! - [`CondensationQuery`]: DAG of SCCs, derived from SCC data
//! - [`ReachabilityQuery`]: Set of nodes reachable from a root set
//!
//! # Relation queries (DB14)
//!
//! - [`CallersQuery`], [`CalleesQuery`]: `callers:X` / `callees:X` semantics
//! - [`ImportsQuery`], [`ExportsQuery`], [`ReferencesQuery`],
//!   [`ImplementsQuery`]: the remaining four relation predicates
//!
//! # Analysis queries (DB14)
//!
//! - [`CyclesQuery`], [`IsInCycleQuery`]: cycle detection backed by
//!   [`SccQuery`]
//! - [`EntryPointsQuery`], [`ReachableFromEntryPointsQuery`],
//!   [`UnusedQuery`], [`IsNodeUnusedQuery`]: unused/dead-code detection
//!
//! Every DB14 query sets `TRACKS_EDGE_REVISION = true` (and
//! `TRACKS_METADATA_REVISION = true` where node metadata influences the
//! result) so the cache is invalidated as the graph evolves.

pub mod address_taken;
pub mod callees;
pub mod callers;
pub mod callsite_promiscuous;
pub mod condensation;
pub mod context_propagation;
pub mod cycles;
pub mod dispatch;
pub mod reachability;
pub mod relation;
pub mod scc;
pub mod type_ids;
pub mod unused;
pub mod unused_post_filter;

pub use address_taken::AddressTakenQuery;
pub use callees::CalleesQuery;
pub use callers::CallersQuery;
pub use callsite_promiscuous::CallsitePromiscuousQuery;
pub use condensation::{CachedCondensation, CondensationKey, CondensationQuery, CondensationValue};
pub use context_propagation::{
    ContextLeak, ContextLeakSet, ContextMode, ContextModeFilter, ContextPropagationKey,
    ContextPropagationQuery, ContextScope,
};
pub use cycles::{
    CycleBounds, CyclesKey, CyclesQuery, CyclesValue, IsInCycleKey, IsInCycleQuery, IsInCycleValue,
};
pub use dispatch::{
    derived_path, load_derived_opportunistic, make_query_db, make_query_db_cold, mcp_callees_query,
    mcp_callers_query, mcp_exports_query, mcp_imports_query, mcp_references_query,
};
pub use reachability::{ReachabilityKey, ReachabilityQuery, ReachableSet};
pub use relation::{
    ExportsQuery, ImplementsQuery, ImportsQuery, ReferencesQuery, RelationKey, RelationKind,
};
pub use scc::{CachedSccData, SccKey, SccQuery, SccValue};
pub use unused::{
    EntryPointsQuery, IsNodeUnusedKey, IsNodeUnusedQuery, IsNodeUnusedValue,
    ReachableFromEntryPointsQuery, UnusedKey, UnusedQuery, UnusedValue,
};
