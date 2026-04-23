//! Top-level library module for the rust_small fixture.
//!
//! Declares the three submodules and re-exports their public entry points.
//! Used by the incremental equivalence harness to exercise cross-file
//! import resolution and the Defines / Contains structural edges.

pub mod math;
pub mod shapes;
pub mod util;

pub use math::add;
pub use shapes::Point;
pub use util::format_point;
