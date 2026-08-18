//! D1: `unsafe_ffi_reach` - unsafe code that crosses a language/service boundary.
//!
//! Universal, parameter-free. Composes P1 (`Predicate::IsUnsafe`), P2
//! (`cross_boundary`), and L1 Primitive A (`emit: EdgeSources`): the result is
//! exactly the unsafe functions that emit at least one cross-boundary edge, so
//! an unsafe function with only intra-language edges is correctly excluded.

use sqry_db::planner::{Direction, Predicate};

use super::{chain, filter, scan, traverse_cross_boundary_emitting};
use crate::dsl::RuleDefinition;
use crate::ir::{RuleNode, TraversalEmit};
use crate::rules::{RuleVariant, ShippedRule};
use crate::witness::RuleSeverity;

/// Stable rule ID.
pub const RULE_ID: &str = "bbnty.security.unsafe_ffi_reach";

/// Fixed traversal depth: exactly 1, a direct outgoing cross-boundary edge from
/// the unsafe function.
///
/// The detector is depth-1 by design and does NOT expose depth as a parameter.
/// The semantics ("an unsafe function that emits a cross-boundary edge") are
/// inherently a direct out-edge. At depth > 1 the `EdgeSources` emit mode would
/// return the sources of every consecutive cross-boundary hop, including safe
/// intermediate nodes (`unsafe U -> safe V -> W` would wrongly report `V`),
/// contradicting the unsafe-function claim. The honest multi-hop alternative
/// (intersecting the traversal sources with the unsafe seed set) is not cleanly
/// expressible, because a `cross_boundary` traversal cannot be a `SetOp` operand.
const DEPTH: u32 = 1;

const VARIANTS: &[RuleVariant] = &[
    RuleVariant::Chain,
    RuleVariant::NodeScan,
    RuleVariant::Filter,
    RuleVariant::EdgeTraversal,
];

/// Builds the `unsafe_ffi_reach` rule definition (depth-1, parameter-free).
///
/// Plan: `Chain[ Chain[NodeScan(any), Filter(IsUnsafe(true))],
/// EdgeTraversal(cross_boundary=Some(true), max_depth=1, emit=EdgeSources) ]`.
/// The inner chain lowers to the unsafe-node set; the outer chain traverses the
/// direct cross-boundary out-edges from it and emits the sources that actually
/// crossed. Because depth is 1, every emitted source IS one of the unsafe seeds.
///
/// Known limitation: `is_unsafe` is currently populated only on Rust free
/// functions, not methods, so today this finds unsafe free-function FFI. The
/// `kind: None` scan is forward-compatible with method population.
#[must_use]
pub fn definition() -> RuleDefinition {
    let unsafe_nodes = RuleNode::Chain {
        steps: vec![scan(None, None), filter(Predicate::IsUnsafe(true))],
    };
    let plan = chain(vec![
        unsafe_nodes,
        traverse_cross_boundary_emitting(Direction::Forward, DEPTH, TraversalEmit::EdgeSources),
    ]);
    RuleDefinition::new(RULE_ID, plan)
        .with_severity(RuleSeverity::Warning)
        .with_description(
            "unsafe code makes a direct cross-language / service / FFI call (audit candidate)",
        )
        .with_remediation(
            "audit the boundary crossing for memory-safety and input-validation invariants",
        )
}

/// The shipped, parameter-free instance.
#[must_use]
pub fn rule() -> ShippedRule {
    ShippedRule {
        definition: definition(),
        title: "Security: unsafe code reaching a language/service boundary",
        methodology: "docs/development/security-addon-2026-07-24/04_DESIGN-L1-security-pack.md D1",
        seed_finding: None,
        variants: VARIANTS,
        requires_beside_cache: false,
        requires_trace_path: false,
        baseline_ms_floor: 1,
    }
}
