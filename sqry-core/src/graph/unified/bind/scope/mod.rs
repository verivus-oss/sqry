//! Phase 2 binding plane — ephemeral `Scope` model.
//!
//! This module hosts the runtime scope arena and (in P2U11) the content-
//! addressed `ScopeStableId` + `ScopeProvenance` + `ScopeProvenanceStore`
//! persistence layer. Later units (P2U03) will add `derive.rs` for the
//! scope-derivation walk and `tree.rs` for the scope chain helpers.

pub mod arena;
pub mod derive;
pub mod provenance;
pub mod tree;

pub use arena::{Scope, ScopeArena, ScopeArenaError, ScopeId, ScopeKind};
pub use provenance::{
    FileStableId, ScopeProvenance, ScopeProvenanceStore, ScopeStableId, compute_scope_stable_id,
};
