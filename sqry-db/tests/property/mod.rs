//! Property-test infrastructure for the WS1 differential harness.
//!
//! This module hosts the proptest-driven well-formed `CodeGraph` generator
//! (DAG unit `U_WS1_3_GRAPH_GEN`, DESIGN §2.2 of
//! `02_DESIGN-graph-fidelity-planner-correctness.md`) and the shared
//! invariant checker the WS1 differential test suites (DESIGN §2.3) rely on.
//!
//! Submodules:
//!
//! * [`graph_gen`] — `well_formed_graph()` strategy + custom shrinker.
//! * [`graph_gen_self_test`] — self-tests proving the generator is sound and
//!   that the shrinker reduces synthetic counter-examples within the
//!   acceptance budget.
//!
//! No production code lives here; the entire module tree is `#[cfg(test)]`.

pub mod graph_gen;
pub mod graph_gen_self_test;
