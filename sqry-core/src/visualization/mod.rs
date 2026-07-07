//! Visualization utilities (diagram rendering, export formats).
//!
//! # Unified Graph Exporters
//!
//! The [`unified`](crate::visualization::unified) module provides visualization exporters that work directly with
//! [`GraphSnapshot`](crate::graph::unified::concurrent::GraphSnapshot):
//!
//! - [`UnifiedDotExporter`](crate::visualization::unified::UnifiedDotExporter) - Graphviz DOT format
//! - [`UnifiedD2Exporter`](crate::visualization::unified::UnifiedD2Exporter) - D2 diagram format
//! - [`UnifiedJsonExporter`](crate::visualization::unified::UnifiedJsonExporter) - JSON for web visualizations
//! - [`UnifiedMermaidExporter`](crate::visualization::unified::UnifiedMermaidExporter) - Mermaid for Markdown
//!
//! A higher-level, seeded architecture export lives in
//! [`archify`](crate::visualization::archify), built on the shared seeded,
//! depth-limited traversal in [`subgraph`](crate::visualization::subgraph).
//!
//! These exporters use the unified graph's edge metadata:
//! - `Calls { argument_count: u8, is_async: bool }`
//! - `Imports { alias: Option<StringId>, is_wildcard: bool }`
//! - `Exports { kind: ExportKind, alias: Option<StringId> }`
//!
//! # Usage Example
//!
//! ```rust,ignore
//! use sqry_core::visualization::unified::{UnifiedDotExporter, DotConfig};
//! let exporter = UnifiedDotExporter::with_config(&graph_snapshot, config);
//! let output = exporter.export();
//! ```

/// Unified graph visualization exporters.
///
/// Use these exporters with [`GraphSnapshot`](crate::graph::unified::concurrent::GraphSnapshot) from the unified graph architecture.
pub mod unified;

/// Shared seeded, depth-limited subgraph traversal (entry-point-agnostic,
/// format-agnostic). Backs the Archify exporter and future seeded exports.
pub mod subgraph;

/// Archify architecture-diagram JSON exporter (seeded, grouped, tier-typed).
pub mod archify;
