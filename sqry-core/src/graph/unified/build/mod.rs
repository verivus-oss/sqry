//! Build pipeline for populating the unified graph from parsed source files.
//!
//! This module implements the 5-pass build pipeline:
//!
//! - **Pass 1**: Extract nodes from AST (`GraphBuilder` emits `NodeEntry` + DEFINES edges)
//! - **Pass 2**: Enrich node metadata (visibility, async, docs) during graph build
//! - **Pass 3**: Create intra-file edges (CALLS, REFERENCES within file)
//! - **Pass 4**: Cross-file linking (IMPORTS, cross-file CALLS via `ExportMap`)
//! - **Pass 5**: Cross-language linking (FFI declarations → C/C++ functions, HTTP requests → endpoints)
//!
//! # Architecture
//!
//! The build pipeline takes parsed ASTs from language plugins and populates
//! the unified graph (`CodeGraph`). Each pass operates on the graph in sequence:
//!
//! ```text
//! LanguagePlugin::build_unified_graph() → nodes + edges
//!            ↓
//!     ┌──────────────────────────────────────┐
//!     │  Pass 1: AST to Nodes                 │
//!     │  - GraphBuilder emits NodeEntry      │
//!     │  - File→Node DEFINES edges           │
//!     └──────────────────────────────────────┘
//!            ↓
//!     ┌──────────────────────────────────────┐
//!     │  Pass 2: Enrichment                   │
//!     │  - Add visibility, async flags       │
//!     │  - Add doc comments                  │
//!     └──────────────────────────────────────┘
//!            ↓
//!     ┌──────────────────────────────────────┐
//!     │  Pass 3: Intra-file Edges             │
//!     │  - CALLS edges within file           │
//!     │  - REFERENCES edges within file      │
//!     └──────────────────────────────────────┘
//!            ↓
//!     ┌──────────────────────────────────────┐
//!     │  Pass 4: Cross-file Linking           │
//!     │  - IMPORTS edges                     │
//!     │  - Cross-file CALLS via ExportMap    │
//!     └──────────────────────────────────────┘
//!            ↓
//!     ┌──────────────────────────────────────┐
//!     │  Pass 5: Cross-language Linking       │
//!     │  - FFI declaration → C/C++ function  │
//!     │  - HTTP request → route endpoint     │
//!     └──────────────────────────────────────┘
//!            ↓
//!     CodeGraph (nodes + edges populated)
//! ```
//!
//! # Transactional Building
//!
//! The [`StagingGraph`] provides transactional semantics for builds:
//! - Collect all changes in a staging buffer
//! - Commit atomically on success
//! - Rollback (discard) on failure, leaving graph unchanged
//!
//! # Incremental Updates
//!
//! The [`incremental`] module provides efficient partial updates:
//! - Remove all nodes from a deleted file
//! - Add edges without full rebuild

pub mod body_hash;
pub mod cancellation;
pub mod entrypoint;
pub mod helper;
pub mod identity;
pub mod incremental;
pub mod parallel_commit;
pub mod pass3_intra;
pub mod pass4_cross;
pub mod pass5_cross_language;
pub mod phase4e_binding;
pub mod phase4e_incremental;
pub mod progress;
pub mod reindex;
pub mod staging;
pub mod test_helpers;
pub(crate) mod unification;

// === Graph Build Phase Names (for progress reporting) ===
// These are `&'static str` constants to avoid allocations in progress events.

/// Phase 1: Extract nodes from symbols (AST to `NodeEntry` conversion)
pub const GRAPH_PHASE_1_NAME: &str = "AST extraction";

/// Phase 2: Enrich node metadata (visibility, async, docs)
pub const GRAPH_PHASE_2_NAME: &str = "Metadata enrichment";

/// Phase 3: Create intra-file edges (calls/references within file)
pub const GRAPH_PHASE_3_NAME: &str = "Intra-file edges";

/// Phase 4: Cross-file linking (imports, cross-file calls)
pub const GRAPH_PHASE_4_NAME: &str = "Cross-file resolution";

/// Phase 5: Cross-language linking (FFI calls, HTTP request→endpoint)
pub const GRAPH_PHASE_5_NAME: &str = "Cross-language linking";

// === Save Component Names (for progress reporting) ===

/// Component: Symbol nodes inside the unified graph snapshot
/// (`<workspace>/.sqry/graph/snapshot.sqry`).
pub const SAVE_COMPONENT_SYMBOLS: &str = "symbols";

/// Component: Trigram index for fuzzy search
pub const SAVE_COMPONENT_TRIGRAMS: &str = "trigrams";

/// Component: Relations (imports, references)
pub const SAVE_COMPONENT_RELATIONS: &str = "relations";

/// Component: Unified code graph snapshot
pub const SAVE_COMPONENT_GRAPH: &str = "unified graph";

// Re-exports
pub use cancellation::CancellationToken;
pub use entrypoint::{
    AnalysisStrategySummary, BuildConfig, BuildResult, GRAPH_FILE_PROCESSING_PHASE,
    build_and_persist_graph, build_and_persist_graph_with_progress, build_unified_graph,
    build_unified_graph_cancellable, build_unified_graph_with_progress,
    build_unified_graph_with_progress_cancellable, persist_and_analyze_graph,
};
pub use helper::{GraphBuildHelper, HelperStats};
pub use identity::{IdentityIndex, IdentityKey};
pub use incremental::{
    IncrementalStats, add_edge_incremental, compute_reverse_dep_closure, incremental_rebuild,
    remove_file_nodes,
};
pub use parallel_commit::{
    ChunkCommitPlan, FilePlan, GlobalOffsets, Phase3Result, compute_commit_plan,
    pending_edges_to_delta, phase2_assign_ranges, phase4_apply_global_remap,
    remap_edge_kind_string_ids, remap_node_entry_global, remap_option_string_id, remap_string_id,
};
// `phase3_parallel_commit` is intentionally *not* re-exported here as
// of Task 4 Step 4 Phase 1. Its new `G: GraphMutationTarget` generic
// bound is `pub(crate)`, which requires the function itself to be
// `pub(crate)`. The only call sites are `entrypoint.rs` and the
// in-module tests, both of which reach the helper through its direct
// module path (`parallel_commit::phase3_parallel_commit`). No external
// crate consumed this function before the migration; the downgrade is
// a no-op for the public API surface.
pub use pass3_intra::{
    IntraFileReference, Pass3Result, Pass3Stats, PendingEdge, UnresolvedRef, pass3_intra_edges,
};
pub use pass4_cross::{ExportMap, Pass4Stats, pass4_cross_file};
pub use pass5_cross_language::{Pass5Stats, link_cross_language_edges};
pub use progress::GraphBuildProgressTracker;
pub use reindex::{ReindexStats, allocate_new_segment, reindex_files};
pub use staging::{StagedEdgeRef, StagedNodeRef, StagingGraph, StagingOp};

// Body hash utilities for duplicate detection
pub use body_hash::{
    build_line_offsets, compute_node_body_hash, extract_node_body, has_valid_body_span,
    node_kind_supports_body_hash, resolve_body_span,
};
