//! Output formatters for sqry-core.
//!
//! This module groups presentation-focused helpers that transform
//! semantic query results into user-consumable formats. The first
//! implementation targets diagram generation (e.g., Mermaid) so the
//! CLI can render call/dependency graphs without external tools.

/// Diagram generation utilities (Mermaid, GraphViz, D2).
pub mod diagram;
