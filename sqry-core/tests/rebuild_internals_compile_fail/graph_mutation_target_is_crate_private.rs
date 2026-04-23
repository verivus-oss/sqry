//! Task 4 Step 4 Phase 1 trybuild fixture: `GraphMutationTarget` is
//! `pub(crate)`, so no external crate can name the trait.
//!
//! The trait is intra-crate infrastructure that generalises Pass 1-5
//! pipeline helpers over `CodeGraph` / `RebuildGraph`. External crates
//! must keep routing through `CodeGraph::clone_for_rebuild` +
//! `RebuildGraph::finalize` to reach the rebuild surface; they must
//! not be able to implement `GraphMutationTarget` themselves
//! (doing so would let a downstream crate smuggle arbitrary mutations
//! into the graph's interior).
//!
//! If this fixture ever *compiles*, the visibility of
//! `GraphMutationTarget` has regressed. Expected compiler diagnostic:
//! `E0603` ("module `mutation_target` is private") or equivalent on
//! the trait path.

use sqry_core::graph::unified::mutation_target::GraphMutationTarget;

fn takes_mutation_target<G: GraphMutationTarget>(_g: &mut G) {}

fn main() {}
