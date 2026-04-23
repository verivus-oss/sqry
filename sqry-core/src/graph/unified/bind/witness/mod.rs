//! Phase 2 binding plane — witness vocabulary.

pub mod render;
pub mod step;

pub use render::WitnessRendering;
pub use step::{
    RejectionReason, ResolutionStep, TieBreakReason, UnresolvedReason, VisibilityReason,
};
