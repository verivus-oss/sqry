//! Relation-predicate derived queries.
//!
//! Shared infrastructure for the six relation predicates used by the query
//! planner. The planner's convention is "filter rows where `<relation>`
//! endpoint matches value" — same shape as [`super::super::planner::execute`]
//! inline `relation_matches` dispatch:
//!
//! | Predicate      | Direction | Edge discriminant            | Endpoint role |
//! |----------------|-----------|------------------------------|---------------|
//! | `callers:X`    | Reverse   | `Calls`                      | Source        |
//! | `callees:X`    | Forward   | `Calls`                      | Target        |
//! | `imports:X`    | Forward   | `Imports`                    | Target        |
//! | `exports:X`    | Both      | `Exports`                    | Either        |
//! | `references:X` | Reverse   | Calls/References/Imports/FFI | Source        |
//! | `implements:X` | Forward   | `Implements`                 | Target        |
//!
//! The [`CallersQuery`] and [`CalleesQuery`] types live in sibling modules
//! ([`super::callers`], [`super::callees`]) but share the [`RelationKey`]
//! shape and helper functions defined here. All six queries set
//! `TRACKS_EDGE_REVISION = true`: any edge change in the graph invalidates
//! the cached result.
//!
//! # What this query migrates from `graph_eval`
//!
//! The planner already had the direction convention before DB14. What DB14
//! adds is the full **language-aware name matching** from
//! [`sqry_core::query::executor::graph_eval`]:
//!
//! * [`entry_query_texts`] produces the interned name, qualified name, and
//!   display-qualified form so segment-aware matching has all three to try.
//! * [`language_aware_segments_match`] supplies the canonicalization fallback
//!   (C# `System.IO` → graph-internal `System::IO`, etc.).
//! * [`extract_method_name`] provides the dynamic-language method-segment
//!   fallback (for `Calls` edges only, mirroring `match_callers`).
//!
//! # Why not include subqueries in the cache key?
//!
//! Subquery values resolve to a `HashSet<NodeId>` at evaluate time. Hashing
//! that set into a stable cache key would defeat the point — the set is
//! already the expensive part. The executor memoises subquery results per
//! plan via its own subquery cache; [`RelationKey`] only covers the
//! name/regex-keyed variants that benefit from cross-plan reuse.
//!
//! # Cross-engine semantics
//!
//! As of DB15, `imports:` is per-node in BOTH engines (sqry-db planner
//! queries here AND `sqry-core::query::executor::graph_eval`). The previous
//! file-scoped `match_imports` was retired so MCP/CLI/LSP all share the
//! planner-canonical semantic per
//! `docs/development/phase-n-structural-semantics/02_DESIGN.md` §6
//! ("Unified Surface Contract"). The convergence test
//! `tests::imports_per_node_semantic_aligned_across_engines` locks the
//! agreement so any future regression surfaces as a test failure.
//!
//! One intentional asymmetry remains for `callers:` / `callees:` direction:
//! the planner treats `<relation>:X` as "filter rows whose `<relation>` set
//! includes `X`", whereas `graph_eval` treats it as "filter rows that ARE in
//! the `<relation>` set of `X`". MCP handlers route through inverted
//! `mcp_callers_query`/`mcp_callees_query` wrappers in
//! [`crate::queries::mcp_wrappers`] so MCP users see the graph_eval-style
//! direction (the contract MCP tool consumers expect) while the planner
//! cache contract is preserved.

use std::collections::HashSet;
use std::sync::Arc;

use regex::RegexBuilder;
use sqry_core::graph::unified::concurrent::GraphSnapshot;
use sqry_core::graph::unified::edge::kind::EdgeKind;
#[cfg(test)]
use sqry_core::graph::unified::edge::kind::ResolvedVia;
use sqry_core::graph::unified::node::id::NodeId;
use sqry_core::graph::unified::storage::arena::NodeEntry;
use sqry_core::query::executor::graph_eval::{
    entry_query_texts, extract_method_name, language_aware_segments_match,
};

use crate::QueryDb;
use crate::dependency::record_file_dep;
use crate::planner::ir::{MatchMode, RegexPattern, StringPattern};
use crate::query::DerivedQuery;

// ============================================================================
// Relation enum + shared key
// ============================================================================

/// The six relation predicates migrated from the planner's inline
/// [`relation_matches`] dispatch.
///
/// Each variant has an associated direction, edge-kind discriminant set, and
/// endpoint role — see the module-level documentation for the table. The
/// discriminator lets every `DerivedQuery` in this module share a single
/// [`compute_relation_source_set`] implementation.
///
/// [`relation_matches`]: super::super::planner::execute
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationKind {
    /// `callers:X` — nodes whose incoming `Calls` edges have a source
    /// matching `X`. A match means `X` is one of the node's callers.
    Callers,
    /// `callees:X` — nodes whose outgoing `Calls` edges have a target
    /// matching `X`. A match means `X` is one of the node's callees.
    Callees,
    /// `imports:X` — nodes whose outgoing `Imports` edges have a target
    /// matching `X`.
    Imports,
    /// `exports:X` — nodes that participate in an `Exports` edge (either
    /// direction) whose *other* endpoint matches `X`.
    Exports,
    /// `references:X` — nodes whose incoming reference edges (`Calls`,
    /// `References`, `Imports`, `FfiCall`) have a source matching `X`.
    References,
    /// `implements:X` — nodes whose outgoing `Implements` edges have a
    /// target matching `X`.
    Implements,
}

/// Cache key for the relation `DerivedQuery` impls.
///
/// Wraps [`StringPattern`] and [`RegexPattern`] from the planner IR so that
/// planner-originating queries and future sqry-core callers share one cache
/// shape. The [`StringPattern::mode`] selects exact / glob / prefix / suffix
/// / contains semantics; [`MatchMode::Exact`] additionally falls through to
/// segment-aware language-aware matching (see [`match_endpoint_name`]).
// PN3 cold-start persistence: both inner types (StringPattern, RegexPattern)
// already derive Serialize/Deserialize from the planner IR.
#[derive(Debug, Clone, Hash, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RelationKey {
    /// Literal / segment / glob / prefix / suffix / contains pattern.
    Pattern(StringPattern),
    /// Regex pattern with compilation flags.
    Regex(RegexPattern),
}

impl RelationKey {
    /// Creates a key for an exact-match (segment-aware) name comparison.
    #[must_use]
    pub fn exact(raw: impl Into<String>) -> Self {
        RelationKey::Pattern(StringPattern::exact(raw))
    }

    /// Creates a key for a case-sensitive regex match.
    #[must_use]
    pub fn regex(pattern: impl Into<String>) -> Self {
        RelationKey::Regex(RegexPattern::new(pattern))
    }
}

// ============================================================================
// Core computation
// ============================================================================

/// Traversal direction used by the relation dispatch.
#[derive(Debug, Clone, Copy)]
pub(super) enum Direction {
    /// Outgoing edges from the node (`snapshot.edges().edges_from(id)`).
    Forward,
    /// Incoming edges to the node (`snapshot.edges().edges_to(id)`).
    Reverse,
    /// Both directions — used by `exports:` to catch edges on either side.
    Both,
}

/// Which edge endpoint is compared against the relation key's name pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EndpointRole {
    /// Edge's source endpoint is the subject under comparison.
    Source,
    /// Edge's target endpoint is the subject under comparison.
    Target,
    /// Whichever endpoint is *not* the query node itself. Handles `exports:`
    /// where the edge may go either direction. Self-loop edges are skipped
    /// to avoid the degenerate case where the node matches its own edge.
    Either,
}

/// Resolves the relation-specific traversal parameters.
///
/// Several variants produce the same `(Direction, EndpointRole)` tuple —
/// e.g. `Callers` and `References` both traverse in reverse and compare on
/// the edge source — but the arms are kept separate so each relation has a
/// single documented entry, matching the planner's `Predicate` variants
/// one-to-one. The `#[allow]` below is deliberate.
#[must_use]
#[allow(clippy::match_same_arms)]
pub(super) fn traversal_for(relation: RelationKind) -> (Direction, EndpointRole) {
    match relation {
        RelationKind::Callers => (Direction::Reverse, EndpointRole::Source),
        RelationKind::Callees => (Direction::Forward, EndpointRole::Target),
        RelationKind::Imports => (Direction::Forward, EndpointRole::Target),
        RelationKind::Exports => (Direction::Both, EndpointRole::Either),
        RelationKind::References => (Direction::Reverse, EndpointRole::Source),
        RelationKind::Implements => (Direction::Forward, EndpointRole::Target),
    }
}

/// Returns `true` if the given edge kind participates in the relation.
#[must_use]
pub(super) fn edge_kind_matches(relation: RelationKind, kind: &EdgeKind) -> bool {
    match relation {
        RelationKind::Callers | RelationKind::Callees => matches!(kind, EdgeKind::Calls { .. }),
        RelationKind::Imports => matches!(kind, EdgeKind::Imports { .. }),
        RelationKind::Exports => matches!(kind, EdgeKind::Exports { .. }),
        RelationKind::References => matches!(
            kind,
            EdgeKind::Calls { .. }
                | EdgeKind::References
                | EdgeKind::Imports { .. }
                | EdgeKind::FfiCall { .. }
        ),
        RelationKind::Implements => matches!(kind, EdgeKind::Implements),
    }
}

/// Computes the sorted, deduplicated set of node IDs that satisfy the
/// relation predicate with the given `key`. Shared by every `DerivedQuery`
/// impl in this module and the sibling `callers.rs` / `callees.rs`.
#[must_use]
pub fn compute_relation_source_set(
    relation: RelationKind,
    key: &RelationKey,
    snapshot: &GraphSnapshot,
) -> Arc<Vec<NodeId>> {
    for (fid, _) in snapshot.file_segments().iter() {
        record_file_dep(fid);
    }

    let matcher = KeyMatcher::build(key);
    let (direction, role) = traversal_for(relation);
    let mut results: Vec<NodeId> = Vec::new();

    for (node_id, entry) in snapshot.nodes().iter() {
        record_file_dep(entry.file);
        // Gate 0d iter-2 fix: skip unified losers from relation
        // query results. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            continue;
        }
        if node_has_matching_endpoint(snapshot, node_id, relation, direction, role, &matcher) {
            results.push(node_id);
        }
    }

    results.sort_unstable_by_key(|id| (id.index(), id.generation()));
    results.dedup();
    Arc::new(results)
}

/// Tests whether a single node matches the relation predicate against a
/// precomputed subquery result set. Used when the value is a
/// [`crate::planner::ir::PredicateValue::Subquery`].
///
/// The `endpoints` set is generic over its hasher so callers don't have to
/// round-trip through the default `RandomState` when their subquery result
/// is already in a `HashSet` with a different hasher.
#[must_use]
pub fn relation_matches_node_via_set<S: std::hash::BuildHasher>(
    relation: RelationKind,
    node_id: NodeId,
    endpoints: &HashSet<NodeId, S>,
    snapshot: &GraphSnapshot,
) -> bool {
    let (direction, role) = traversal_for(relation);
    for edge in neighbours(snapshot, node_id, direction) {
        if !edge_kind_matches(relation, &edge.kind) {
            continue;
        }
        let endpoint = match role {
            EndpointRole::Source => edge.source,
            EndpointRole::Target => edge.target,
            EndpointRole::Either => {
                if edge.source == node_id {
                    edge.target
                } else {
                    edge.source
                }
            }
        };
        if role == EndpointRole::Either && endpoint == node_id {
            // Degenerate self-loop under Either — skip to avoid matching the
            // query node against itself through its own edge.
            continue;
        }
        if endpoints.contains(&endpoint) {
            return true;
        }
    }
    false
}

// ============================================================================
// Per-node probing
// ============================================================================

fn node_has_matching_endpoint(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    relation: RelationKind,
    direction: Direction,
    role: EndpointRole,
    matcher: &KeyMatcher,
) -> bool {
    // Imports fast-path: an `Import` node whose own name matches the key
    // is a hit even with no outgoing edges. Mirrors the
    // `entry.kind == NodeKind::Import` branch in
    // `graph_eval::match_imports`. DB14's edge-only model missed this; the
    // followup test `imports_per_node_semantic_aligned_across_engines`
    // catches the divergence.
    if relation == RelationKind::Imports
        && let Some(entry) = snapshot.nodes().get(node_id)
        && entry.kind == sqry_core::graph::unified::node::NodeKind::Import
        && match_endpoint_name(snapshot, entry, matcher)
    {
        return true;
    }

    for edge in neighbours(snapshot, node_id, direction) {
        if !edge_kind_matches(relation, &edge.kind) {
            continue;
        }
        let endpoint_id = match role {
            EndpointRole::Source => edge.source,
            EndpointRole::Target => edge.target,
            EndpointRole::Either => {
                if edge.source == node_id {
                    edge.target
                } else {
                    edge.source
                }
            }
        };
        if role == EndpointRole::Either && endpoint_id == node_id {
            continue;
        }
        let Some(endpoint_entry) = snapshot.nodes().get(endpoint_id) else {
            continue;
        };
        if match_endpoint_name(snapshot, endpoint_entry, matcher) {
            return true;
        }
        // Dynamic-language fallback: for `Calls` edges the legacy
        // `graph_eval::match_callers` also accepts matching on the trailing
        // method segment (`Player::takeDamage` ↔ `Enemy::takeDamage`). Keep
        // that in place so migrating Ruby/Python dispatch-style call sites
        // stays covered.
        if matches!(relation, RelationKind::Callers | RelationKind::Callees)
            && method_segment_matches(snapshot, endpoint_entry, matcher)
        {
            return true;
        }
        // Imports edges carry alias and wildcard metadata that
        // `graph_eval::import_edge_matches` honours (DB15 reconciled the
        // two engines on per-node semantics; this preserves alias /
        // wildcard parity so the convergence test
        // `imports_per_node_semantic_aligned_across_engines` passes for
        // every branch of `match_imports`, not just the target-name
        // branch).
        if relation == RelationKind::Imports
            && imports_alias_or_wildcard_matches(snapshot, &edge, matcher)
        {
            return true;
        }
    }
    false
}

/// Returns `true` if an `Imports` edge matches the key via alias text or
/// the `*` wildcard, mirroring `graph_eval::import_edge_matches`.
///
/// Only `Exact`-mode literal patterns participate in the wildcard /
/// alias fast paths; regex / glob / prefix-suffix-contains modes already
/// flow through [`match_endpoint_name`] for the alias text by virtue of
/// the alias being interned. We only run the fast paths for `Exact` mode
/// because that is the convention `graph_eval::import_edge_matches`
/// uses.
fn imports_alias_or_wildcard_matches(
    snapshot: &GraphSnapshot,
    edge: &sqry_core::graph::unified::edge::store::StoreEdgeRef,
    matcher: &KeyMatcher,
) -> bool {
    let EdgeKind::Imports { alias, is_wildcard } = &edge.kind else {
        return false;
    };
    let KeyMatcher::Pattern(pattern) = matcher else {
        return false;
    };
    if !matches!(pattern.mode, MatchMode::Exact) {
        return false;
    }
    if *is_wildcard && pattern.raw == "*" {
        return true;
    }
    let Some(alias_id) = alias else {
        return false;
    };
    let Some(alias_str) = snapshot.strings().resolve(*alias_id) else {
        return false;
    };
    // Match alias against the requested key; mirrors graph_eval's
    // `import_text_matches`-anchored alias check (substring + canonical
    // form). For the per-node planner contract, an exact substring match
    // on the alias text is sufficient — the alias IS the importer's
    // local name for the imported symbol.
    alias_str.as_ref().contains(pattern.raw.as_str())
}

fn neighbours(
    snapshot: &GraphSnapshot,
    node_id: NodeId,
    direction: Direction,
) -> Vec<sqry_core::graph::unified::edge::store::StoreEdgeRef> {
    match direction {
        Direction::Forward => snapshot.edges().edges_from(node_id),
        Direction::Reverse => snapshot.edges().edges_to(node_id),
        Direction::Both => {
            let mut out = snapshot.edges().edges_from(node_id);
            out.extend(snapshot.edges().edges_to(node_id));
            out
        }
    }
}

// ============================================================================
// Key matcher — compile once per query invocation
// ============================================================================

/// Compiled representation of a [`RelationKey`]. Regex compilation fails
/// silently into a rejecting matcher — relation predicates should return an
/// empty set rather than panic on malformed user input.
enum KeyMatcher {
    Pattern(StringPattern),
    Regex(regex::Regex),
    RegexReject,
}

impl KeyMatcher {
    fn build(key: &RelationKey) -> Self {
        match key {
            RelationKey::Pattern(pattern) => KeyMatcher::Pattern(pattern.clone()),
            RelationKey::Regex(pattern) => {
                let mut builder = RegexBuilder::new(&pattern.pattern);
                builder
                    .case_insensitive(pattern.flags.case_insensitive)
                    .multi_line(pattern.flags.multiline)
                    .dot_matches_new_line(pattern.flags.dot_all)
                    .size_limit(1 << 20)
                    .dfa_size_limit(1 << 20);
                builder
                    .build()
                    .map(KeyMatcher::Regex)
                    .unwrap_or(KeyMatcher::RegexReject)
            }
        }
    }
}

/// Tests whether any of `entry`'s query texts match the matcher. Exact-mode
/// literal patterns route through `language_aware_segments_match` so the
/// legacy `graph_eval` segment semantics survive the migration; non-`Exact`
/// literal modes use simple string operations; regex uses the compiled
/// `regex::Regex`.
fn match_endpoint_name(snapshot: &GraphSnapshot, entry: &NodeEntry, matcher: &KeyMatcher) -> bool {
    let texts = entry_query_texts(snapshot, entry);
    if texts.is_empty() {
        return false;
    }
    match matcher {
        KeyMatcher::Pattern(pattern) => match pattern.mode {
            MatchMode::Exact => texts.iter().any(|candidate| {
                language_aware_segments_match(snapshot, entry.file, candidate, &pattern.raw)
            }),
            MatchMode::Glob => globset::Glob::new(&pattern.raw).is_ok_and(|g| {
                let m = g.compile_matcher();
                texts.iter().any(|candidate| m.is_match(candidate))
            }),
            MatchMode::Prefix => {
                let needle = case_adjust(&pattern.raw, pattern.case_insensitive);
                texts.iter().any(|candidate| {
                    case_adjust(candidate, pattern.case_insensitive).starts_with(&needle)
                })
            }
            MatchMode::Suffix => {
                let needle = case_adjust(&pattern.raw, pattern.case_insensitive);
                texts.iter().any(|candidate| {
                    case_adjust(candidate, pattern.case_insensitive).ends_with(&needle)
                })
            }
            MatchMode::Contains => {
                let needle = case_adjust(&pattern.raw, pattern.case_insensitive);
                texts.iter().any(|candidate| {
                    case_adjust(candidate, pattern.case_insensitive).contains(&needle)
                })
            }
        },
        KeyMatcher::Regex(re) => texts.iter().any(|candidate| re.is_match(candidate)),
        KeyMatcher::RegexReject => false,
    }
}

fn case_adjust(s: &str, case_insensitive: bool) -> String {
    if case_insensitive {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

fn method_segment_matches(
    snapshot: &GraphSnapshot,
    entry: &NodeEntry,
    matcher: &KeyMatcher,
) -> bool {
    let KeyMatcher::Pattern(pattern) = matcher else {
        return false;
    };
    if !matches!(pattern.mode, MatchMode::Exact) {
        return false;
    }
    let Some(method) = extract_method_name(&pattern.raw) else {
        return false;
    };
    entry_query_texts(snapshot, entry)
        .iter()
        .filter_map(|candidate| extract_method_name(candidate))
        .any(|candidate_method| candidate_method == method)
}

// ============================================================================
// DerivedQuery impls — 4 of the 6 relations live here. Callers/Callees live
// in sibling modules so the file layout matches the DAG's `files_create`.
// ============================================================================

/// `imports:X` — nodes whose outgoing `Imports` edges have a target
/// matching `X`. See [`RelationKind::Imports`].
pub struct ImportsQuery;

impl DerivedQuery for ImportsQuery {
    type Key = RelationKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::IMPORTS;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &RelationKey, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        compute_relation_source_set(RelationKind::Imports, key, snapshot)
    }
}

/// `exports:X` — nodes participating in an `Exports` edge whose other
/// endpoint matches `X`. See [`RelationKind::Exports`].
pub struct ExportsQuery;

impl DerivedQuery for ExportsQuery {
    type Key = RelationKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::EXPORTS;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &RelationKey, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        compute_relation_source_set(RelationKind::Exports, key, snapshot)
    }
}

/// `references:X` — nodes with incoming reference edges (`Calls`,
/// `References`, `Imports`, `FfiCall`) whose source matches `X`.
pub struct ReferencesQuery;

impl DerivedQuery for ReferencesQuery {
    type Key = RelationKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::REFERENCES;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &RelationKey, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        compute_relation_source_set(RelationKind::References, key, snapshot)
    }
}

/// `implements:X` — nodes whose outgoing `Implements` edges reach a target
/// matching `X`.
pub struct ImplementsQuery;

impl DerivedQuery for ImplementsQuery {
    type Key = RelationKey;
    type Value = Arc<Vec<NodeId>>;
    const QUERY_TYPE_ID: u32 = crate::queries::type_ids::IMPLEMENTS;
    const TRACKS_EDGE_REVISION: bool = true;

    fn execute(key: &RelationKey, _db: &QueryDb, snapshot: &GraphSnapshot) -> Arc<Vec<NodeId>> {
        compute_relation_source_set(RelationKind::Implements, key, snapshot)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryDbConfig;
    use sqry_core::graph::unified::concurrent::CodeGraph;
    use sqry_core::graph::unified::edge::kind::ExportKind;
    use sqry_core::graph::unified::node::kind::NodeKind;
    use std::path::Path;
    use std::sync::Arc;

    fn alloc_function(graph: &mut CodeGraph, name: &str, file: &Path) -> NodeId {
        let name_id = graph.strings_mut().intern(name).unwrap();
        let file_id = graph.files_mut().register(file).unwrap();
        let entry =
            NodeEntry::new(NodeKind::Function, name_id, file_id).with_qualified_name(name_id);
        graph.nodes_mut().alloc(entry).unwrap()
    }

    fn add_calls_edge(graph: &mut CodeGraph, src: NodeId, tgt: NodeId) {
        let file_id = graph.nodes().get(src).unwrap().file;
        graph.edges_mut().add_edge(
            src,
            tgt,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            file_id,
        );
    }

    fn build_db_for_graph(graph: CodeGraph) -> QueryDb {
        let snapshot = Arc::new(graph.snapshot());
        let mut db = QueryDb::new(snapshot, QueryDbConfig::default());
        db.register::<ImportsQuery>();
        db.register::<ExportsQuery>();
        db.register::<ReferencesQuery>();
        db.register::<ImplementsQuery>();
        db
    }

    #[test]
    fn callers_relation_returns_nodes_called_by_matching_source() {
        // main --Calls--> {a, b}; `callers:main` = {a, b} because main is
        // listed in each callee's `callers` set.
        let mut graph = CodeGraph::new();
        let path = Path::new("main.rs");
        let main_fn = alloc_function(&mut graph, "main", path);
        let a = alloc_function(&mut graph, "a", path);
        let b = alloc_function(&mut graph, "b", path);
        add_calls_edge(&mut graph, main_fn, a);
        add_calls_edge(&mut graph, main_fn, b);

        let snapshot = Arc::new(graph.snapshot());
        let out = compute_relation_source_set(
            RelationKind::Callers,
            &RelationKey::exact("main"),
            &snapshot,
        );
        let ids: Vec<NodeId> = out.as_ref().clone();
        assert!(ids.contains(&a));
        assert!(ids.contains(&b));
        assert!(!ids.contains(&main_fn));
    }

    #[test]
    fn callees_relation_returns_nodes_that_call_matching_target() {
        // main --Calls--> parse_expr; `callees:parse_expr` = {main}.
        let mut graph = CodeGraph::new();
        let path = Path::new("main.rs");
        let main_fn = alloc_function(&mut graph, "main", path);
        let parse_expr = alloc_function(&mut graph, "parse_expr", path);
        add_calls_edge(&mut graph, main_fn, parse_expr);

        let snapshot = Arc::new(graph.snapshot());
        let out = compute_relation_source_set(
            RelationKind::Callees,
            &RelationKey::exact("parse_expr"),
            &snapshot,
        );
        assert_eq!(out.as_ref(), &vec![main_fn]);
    }

    #[test]
    fn references_relation_matches_incoming_reference_edges() {
        // caller --References--> target; `references:caller` = {target}.
        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let file_id = graph.files_mut().register(path).unwrap();
        let caller_name = graph.strings_mut().intern("caller").unwrap();
        let target_name = graph.strings_mut().intern("target").unwrap();
        let unrelated_name = graph.strings_mut().intern("unrelated").unwrap();

        let caller = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, caller_name, file_id)
                    .with_qualified_name(caller_name),
            )
            .unwrap();
        let target = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, target_name, file_id)
                    .with_qualified_name(target_name),
            )
            .unwrap();
        let unrelated = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, unrelated_name, file_id)
                    .with_qualified_name(unrelated_name),
            )
            .unwrap();

        graph
            .edges_mut()
            .add_edge(caller, target, EdgeKind::References, file_id);

        let snapshot = Arc::new(graph.snapshot());
        let out = compute_relation_source_set(
            RelationKind::References,
            &RelationKey::exact("caller"),
            &snapshot,
        );
        assert_eq!(out.as_ref(), &vec![target]);
        assert!(!out.contains(&unrelated));
    }

    #[test]
    fn implements_relation_matches_outgoing_implements_edge() {
        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let name_id = graph.strings_mut().intern("MyStruct").unwrap();
        let trait_name = graph.strings_mut().intern("Drawable").unwrap();
        let file_id = graph.files_mut().register(path).unwrap();
        let s = graph
            .nodes_mut()
            .alloc(NodeEntry::new(NodeKind::Struct, name_id, file_id).with_qualified_name(name_id))
            .unwrap();
        let t = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Trait, trait_name, file_id)
                    .with_qualified_name(trait_name),
            )
            .unwrap();
        graph
            .edges_mut()
            .add_edge(s, t, EdgeKind::Implements, file_id);

        let snapshot = Arc::new(graph.snapshot());
        let out = compute_relation_source_set(
            RelationKind::Implements,
            &RelationKey::exact("Drawable"),
            &snapshot,
        );
        assert_eq!(out.as_ref(), &vec![s]);
    }

    #[test]
    fn exports_relation_matches_either_direction() {
        // run_fn --Exports--> main_fn. Under the Either/Both convention,
        // `exports:main` should return run_fn (its export target is named
        // "main"), and `exports:run_fn` should return main_fn (its export
        // source is named "run_fn").
        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let file_id = graph.files_mut().register(path).unwrap();
        let run_name = graph.strings_mut().intern("run_fn").unwrap();
        let main_name = graph.strings_mut().intern("main").unwrap();
        let run_fn = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, run_name, file_id).with_qualified_name(run_name),
            )
            .unwrap();
        let main_fn = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, main_name, file_id)
                    .with_qualified_name(main_name),
            )
            .unwrap();
        graph.edges_mut().add_edge(
            run_fn,
            main_fn,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            file_id,
        );

        let snapshot = Arc::new(graph.snapshot());
        let out_main = compute_relation_source_set(
            RelationKind::Exports,
            &RelationKey::exact("main"),
            &snapshot,
        );
        assert_eq!(out_main.as_ref(), &vec![run_fn]);
        let out_run = compute_relation_source_set(
            RelationKind::Exports,
            &RelationKey::exact("run_fn"),
            &snapshot,
        );
        assert_eq!(out_run.as_ref(), &vec![main_fn]);
    }

    #[test]
    fn imports_relation_matches_outgoing_import_target() {
        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let file_id = graph.files_mut().register(path).unwrap();
        let importer_name = graph.strings_mut().intern("importer").unwrap();
        let target_name = graph.strings_mut().intern("serde").unwrap();
        let importer = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, importer_name, file_id)
                    .with_qualified_name(importer_name),
            )
            .unwrap();
        let target = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Module, target_name, file_id)
                    .with_qualified_name(target_name),
            )
            .unwrap();
        graph.edges_mut().add_edge(
            importer,
            target,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            file_id,
        );

        let snapshot = Arc::new(graph.snapshot());
        let out = compute_relation_source_set(
            RelationKind::Imports,
            &RelationKey::exact("serde"),
            &snapshot,
        );
        assert_eq!(out.as_ref(), &vec![importer]);
    }

    #[test]
    fn derived_query_is_cached_and_invalidated_on_edge_bump() {
        let mut graph = CodeGraph::new();
        let path = Path::new("main.rs");
        let a = alloc_function(&mut graph, "a", path);
        let b = alloc_function(&mut graph, "b", path);
        add_calls_edge(&mut graph, a, b);

        let db = build_db_for_graph(graph);
        let first = db.get::<ImplementsQuery>(&RelationKey::exact("nothing"));
        assert!(first.as_ref().is_empty());
        let second = db.get::<ImplementsQuery>(&RelationKey::exact("nothing"));
        assert!(Arc::ptr_eq(&first, &second));

        db.bump_edge_revision();
        let third = db.get::<ImplementsQuery>(&RelationKey::exact("nothing"));
        assert!(!Arc::ptr_eq(&first, &third));
    }

    #[test]
    fn relation_matches_node_via_set_honours_direction() {
        let mut graph = CodeGraph::new();
        let path = Path::new("main.rs");
        let a = alloc_function(&mut graph, "a", path);
        let b = alloc_function(&mut graph, "b", path);
        add_calls_edge(&mut graph, a, b);

        let snapshot = graph.snapshot();
        // Callers(X) semantics: does b have an incoming Calls from an
        // endpoint in the set? Set = {a}. Yes → true.
        let mut endpoints = HashSet::new();
        endpoints.insert(a);
        assert!(relation_matches_node_via_set(
            RelationKind::Callers,
            b,
            &endpoints,
            &snapshot
        ));
        // a has no incoming Calls, so its Callers set is empty → false.
        assert!(!relation_matches_node_via_set(
            RelationKind::Callers,
            a,
            &endpoints,
            &snapshot
        ));
    }

    /// Codex review (2026-04-15) — explicit regression for the
    /// `EndpointRole::Either` self-loop skip on `exports:`.
    ///
    /// A node with an `Exports` self-edge must not satisfy `exports:<own
    /// name>` purely through that self-edge. The original `match_*`
    /// semantics rejected it (otherwise every exported symbol becomes its
    /// own export target and the predicate degenerates), and the planner's
    /// `relation_matches` always skipped self-loops under `Either`. DB14
    /// must preserve both behaviours in both code paths.
    #[test]
    fn exports_self_loop_is_rejected_in_keymatcher_path() {
        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let file_id = graph.files_mut().register(path).unwrap();
        let sym_name = graph.strings_mut().intern("self_export").unwrap();
        let sym = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, sym_name, file_id).with_qualified_name(sym_name),
            )
            .unwrap();
        graph.edges_mut().add_edge(
            sym,
            sym,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            file_id,
        );

        let snapshot = Arc::new(graph.snapshot());
        let out = compute_relation_source_set(
            RelationKind::Exports,
            &RelationKey::exact("self_export"),
            &snapshot,
        );
        assert!(
            out.as_ref().is_empty(),
            "self-export must not satisfy exports:<own name> (regression \
             for the Either-role self-loop skip)"
        );
    }

    #[test]
    fn exports_self_loop_is_rejected_in_subquery_set_path() {
        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let file_id = graph.files_mut().register(path).unwrap();
        let sym_name = graph.strings_mut().intern("self_export").unwrap();
        let sym = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, sym_name, file_id).with_qualified_name(sym_name),
            )
            .unwrap();
        graph.edges_mut().add_edge(
            sym,
            sym,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            file_id,
        );

        let snapshot = graph.snapshot();
        let mut endpoints = HashSet::new();
        endpoints.insert(sym);
        assert!(
            !relation_matches_node_via_set(RelationKind::Exports, sym, &endpoints, &snapshot,),
            "subquery-set path must also skip the Either-role self-loop"
        );
    }

    /// Codex review (2026-04-15) — semantic-divergence freeze for `imports:`.
    ///
    /// Locks the post-DB15 agreement between sqry-db's per-node
    /// `ImportsQuery` and the `sqry-core::query::executor::graph_eval`
    /// engine. Both must produce identical match decisions for the same
    /// `(node, target_module)` pair. If a future change re-introduces the
    /// pre-DB15 file-scoped behavior in either engine, this test fails so
    /// the divergence cannot ship silently.
    ///
    /// Coverage matrix (post-DB15-followup):
    /// * outgoing `Imports` edge with matching target text — both engines must agree
    /// * outgoing `Imports` edge with matching alias text — both engines must agree
    /// * outgoing `Imports` edge with `is_wildcard = true` and `*` key — both engines must agree
    /// * an `Import` *node* whose own name matches the key (the
    ///   `entry.kind == NodeKind::Import` fast-path in
    ///   `graph_eval::match_imports` and the same fall-through in
    ///   `compute_relation_source_set`) — both engines must agree
    /// * negative case: a non-`Import` node in the same file with no
    ///   outgoing `Imports` edge — both engines must reject (would have
    ///   matched under the pre-DB15 file-scoped semantic)
    #[test]
    fn imports_per_node_semantic_aligned_across_engines() {
        use sqry_core::query::executor::graph_eval::{import_edge_matches, import_entry_matches};

        let mut graph = CodeGraph::new();
        let path = Path::new("lib.rs");
        let file_id = graph.files_mut().register(path).unwrap();
        let importer_name = graph.strings_mut().intern("importer").unwrap();
        let alias_importer_name = graph.strings_mut().intern("alias_importer").unwrap();
        let wildcard_importer_name = graph.strings_mut().intern("wildcard_importer").unwrap();
        let import_node_name = graph.strings_mut().intern("serde").unwrap();
        let unrelated_name = graph.strings_mut().intern("unrelated").unwrap();
        let target_name = graph.strings_mut().intern("serde").unwrap();
        let aliased_target_name = graph.strings_mut().intern("private_target").unwrap();
        let alias_text = graph.strings_mut().intern("serde").unwrap();
        let wildcard_target_name = graph.strings_mut().intern("anything").unwrap();

        // Function with an outgoing Imports -> serde edge.
        let importer = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, importer_name, file_id)
                    .with_qualified_name(importer_name),
            )
            .unwrap();
        // Function whose Imports edge target name does NOT match, but
        // whose alias does — exercises the alias-match branch.
        let alias_importer = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, alias_importer_name, file_id)
                    .with_qualified_name(alias_importer_name),
            )
            .unwrap();
        // Function with a wildcard Imports edge — matches `*` key.
        let wildcard_importer = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, wildcard_importer_name, file_id)
                    .with_qualified_name(wildcard_importer_name),
            )
            .unwrap();
        // Bare Import node whose own name is `serde` — exercises the
        // `entry.kind == NodeKind::Import` branch in match_imports.
        let import_node = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Import, import_node_name, file_id)
                    .with_qualified_name(import_node_name),
            )
            .unwrap();
        // Same-file node with no outgoing Imports — pre-DB15 file-scoped
        // semantic would have matched this; post-DB15 per-node must reject.
        let unrelated = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Function, unrelated_name, file_id)
                    .with_qualified_name(unrelated_name),
            )
            .unwrap();
        let target = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Module, target_name, file_id)
                    .with_qualified_name(target_name),
            )
            .unwrap();
        let aliased_target = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Module, aliased_target_name, file_id)
                    .with_qualified_name(aliased_target_name),
            )
            .unwrap();
        let wildcard_target = graph
            .nodes_mut()
            .alloc(
                NodeEntry::new(NodeKind::Module, wildcard_target_name, file_id)
                    .with_qualified_name(wildcard_target_name),
            )
            .unwrap();

        // importer --Imports{None}--> serde
        graph.edges_mut().add_edge(
            importer,
            target,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            file_id,
        );
        // alias_importer --Imports{alias=serde}--> private_target
        graph.edges_mut().add_edge(
            alias_importer,
            aliased_target,
            EdgeKind::Imports {
                alias: Some(alias_text),
                is_wildcard: false,
            },
            file_id,
        );
        // wildcard_importer --Imports{wildcard}--> anything
        graph.edges_mut().add_edge(
            wildcard_importer,
            wildcard_target,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: true,
            },
            file_id,
        );

        let snapshot = Arc::new(graph.snapshot());

        let sqry_db_matches_for = |key: &str| -> Arc<Vec<NodeId>> {
            compute_relation_source_set(RelationKind::Imports, &RelationKey::exact(key), &snapshot)
        };

        // Per-node graph_eval check covering ALL of match_imports's
        // branches (Import node fast-path + outgoing-edge branch).
        let snapshot_ref: &GraphSnapshot = snapshot.as_ref();
        let graph_eval_matches = |node_id: NodeId, key: &str| -> bool {
            let entry = match snapshot_ref.nodes().get(node_id) {
                Some(e) => e,
                None => return false,
            };
            if entry.kind == NodeKind::Import && import_entry_matches(snapshot_ref, entry, key) {
                return true;
            }
            snapshot_ref
                .edges()
                .edges_from(node_id)
                .iter()
                .any(|edge| import_edge_matches(snapshot_ref, edge, key))
        };

        // (key, node, label, expected_match)
        let cases: &[(&str, NodeId, &str, bool)] = &[
            ("serde", importer, "importer (target match)", true),
            (
                "serde",
                alias_importer,
                "alias_importer (alias match)",
                true,
            ),
            (
                "*",
                wildcard_importer,
                "wildcard_importer (wildcard *)",
                true,
            ),
            ("serde", import_node, "import_node (Import fast-path)", true),
            ("serde", unrelated, "unrelated (no outgoing Imports)", false),
            ("serde", target, "target (no outgoing Imports)", false),
        ];

        for &(key, node, label, expected) in cases {
            let db_set = sqry_db_matches_for(key);
            let db_hit = db_set.contains(&node);
            let ge_hit = graph_eval_matches(node, key);
            assert_eq!(
                db_hit, ge_hit,
                "{label}: sqry-db ImportsQuery and graph_eval::match_imports \
                 must agree under per-node semantics for key={key:?} \
                 (db_hit={db_hit}, ge_hit={ge_hit})"
            );
            assert_eq!(
                db_hit, expected,
                "{label}: expected match={expected} for key={key:?}"
            );
        }
    }
}

// ============================================================================
// PN3 serde roundtrip tests
// ============================================================================

#[cfg(test)]
mod serde_roundtrip {
    use super::*;
    use crate::planner::ir::{MatchMode, RegexFlags, RegexPattern, StringPattern};
    use postcard::{from_bytes, to_allocvec};

    #[test]
    fn relation_key_pattern_roundtrip() {
        let original = RelationKey::Pattern(StringPattern {
            raw: "my_function".to_string(),
            mode: MatchMode::Exact,
            case_insensitive: false,
        });
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: RelationKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }

    #[test]
    fn relation_key_regex_roundtrip() {
        let original = RelationKey::Regex(RegexPattern {
            pattern: "fn_.*".to_string(),
            flags: RegexFlags {
                case_insensitive: true,
                multiline: false,
                dot_all: false,
            },
        });
        let bytes = to_allocvec(&original).expect("serialize failed");
        let decoded: RelationKey = from_bytes(&bytes).expect("deserialize failed");
        assert_eq!(decoded, original);
    }
}
