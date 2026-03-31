//! Cacheable graph node summary for AST cache.
//!
//! This module defines a lightweight graph-native representation optimized for
//! caching. Unlike full graph storage, the cached summary contains only fields
//! needed to rebuild query-facing metadata without Node types.
//!
//! # Size Budget
//!
//! The cached summary targets ≤256 bytes per node (postcard serialization).
//! This keeps memory footprint reasonable for the 50 MB default cache.
//!
//! # Excluded Fields
//!
//! The following fields from `NodeEntry` are excluded from the cache payload:
//! - **Docstrings**: Can be large (100-1000 bytes), rarely needed for search
//! - **Body hashes**: Recomputed during graph build when needed
//! - **Graph IDs**: Node IDs are graph-specific and not stable across runs
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::cache::GraphNodeSummary;
//! use sqry_core::graph::unified::CodeGraph;
//!
//! let graph = CodeGraph::new();
//! let entry = graph.nodes().get(/* node id */).unwrap();
//! let summary = GraphNodeSummary::from_entry(entry, &graph).unwrap();
//!
//! // Serialize for cache storage
//! let bytes = postcard::to_allocvec(&summary)?;
//! assert!(bytes.len() <= 256);
//! ```

use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::node::NodeKind;
use crate::graph::unified::storage::arena::NodeEntry;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

/// Lightweight node summary for cache storage.
///
/// Contains only the essential fields needed for semantic code search:
/// - Node identification (name, kind)
/// - Location information (file path, line/column ranges)
/// - Optional metadata (qualified name, visibility, signature)
///
/// # Serialization
///
/// Uses postcard for compact binary serialization. Target size: ≤256 bytes.
///
/// # Memory Optimization
///
/// Uses `Arc<str>` for string fields and `Arc<Path>` for paths to enable
/// efficient sharing when multiple cache entries reference the same data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNodeSummary {
    /// Node name (shared via Arc)
    #[serde(
        serialize_with = "serialize_arc_str",
        deserialize_with = "deserialize_arc_str"
    )]
    pub name: Arc<str>,

    /// Node kind (function, class, etc.)
    pub node_kind: NodeKind,

    /// File path where node is defined (shared via Arc)
    #[serde(
        serialize_with = "serialize_arc_path",
        deserialize_with = "deserialize_arc_path"
    )]
    pub file_path: Arc<Path>,

    /// Line number where node starts (1-based)
    pub start_line: u32,

    /// Column where node starts (0-based)
    pub start_column: u32,

    /// Line number where node ends (1-based)
    pub end_line: u32,

    /// Column where node ends (0-based)
    pub end_column: u32,

    /// Fully qualified name (e.g., "`Module::Class::method`")
    #[serde(
        serialize_with = "serialize_option_arc_str",
        deserialize_with = "deserialize_option_arc_str"
    )]
    pub qualified_name: Option<Arc<str>>,

    /// Visibility modifier (public, private, protected, etc.)
    #[serde(
        serialize_with = "serialize_option_arc_str",
        deserialize_with = "deserialize_option_arc_str"
    )]
    pub visibility: Option<Arc<str>>,

    /// Optional signature/type information
    #[serde(
        serialize_with = "serialize_option_arc_str",
        deserialize_with = "deserialize_option_arc_str"
    )]
    pub signature: Option<Arc<str>>,

    /// Whether this is an async function/method.
    pub is_async: bool,

    /// Whether this is a static member.
    pub is_static: bool,
}

// Custom serialization for Arc<str>
fn serialize_arc_str<S>(arc: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(arc)
}

fn deserialize_arc_str<'de, D>(deserializer: D) -> Result<Arc<str>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(Arc::from(s.as_str()))
}

// Custom serialization for Option<Arc<str>>
#[allow(clippy::ref_option)] // serde serialize_with requires &Option<T>
fn serialize_option_arc_str<S>(opt: &Option<Arc<str>>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match opt {
        Some(arc) => serializer.serialize_some(arc.as_ref()),
        None => serializer.serialize_none(),
    }
}

fn deserialize_option_arc_str<'de, D>(deserializer: D) -> Result<Option<Arc<str>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    Ok(opt.map(|s| Arc::from(s.as_str())))
}

// Custom serialization for Arc<Path>
fn serialize_arc_path<S>(arc: &Arc<Path>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(arc.to_str().unwrap_or(""))
}

fn deserialize_arc_path<'de, D>(deserializer: D) -> Result<Arc<Path>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    Ok(Arc::from(Path::new(&s)))
}

impl GraphNodeSummary {
    /// Build a summary from a graph node entry.
    ///
    /// Returns `None` if required string or file resolution fails.
    #[must_use]
    pub fn from_entry(entry: &NodeEntry, graph: &CodeGraph) -> Option<Self> {
        let strings = graph.strings();
        let files = graph.files();

        let name = strings.resolve(entry.name)?;
        let file_path = files.resolve(entry.file)?;
        let qualified_name = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .map(|value| Arc::from(value.as_ref()));
        let visibility = entry
            .visibility
            .and_then(|id| strings.resolve(id))
            .map(|value| Arc::from(value.as_ref()));
        let signature = entry
            .signature
            .and_then(|id| strings.resolve(id))
            .map(|value| Arc::from(value.as_ref()));

        Some(Self {
            name: Arc::from(name.as_ref()),
            node_kind: entry.kind,
            file_path: Arc::from(file_path.as_ref()),
            start_line: entry.start_line,
            start_column: entry.start_column,
            end_line: entry.end_line,
            end_column: entry.end_column,
            qualified_name,
            visibility,
            signature,
            is_async: entry.is_async,
            is_static: entry.is_static,
        })
    }

    /// Create a summary from explicit fields (useful for tests).
    #[must_use]
    pub fn new(
        name: impl Into<Arc<str>>,
        node_kind: NodeKind,
        file_path: impl Into<Arc<Path>>,
        start_line: u32,
        start_column: u32,
        end_line: u32,
        end_column: u32,
    ) -> Self {
        Self {
            name: name.into(),
            node_kind,
            file_path: file_path.into(),
            start_line,
            start_column,
            end_line,
            end_column,
            qualified_name: None,
            visibility: None,
            signature: None,
            is_async: false,
            is_static: false,
        }
    }

    /// Estimate the serialized size in bytes (postcard format).
    ///
    /// Returns the actual postcard-serialized size of this summary.
    /// Used for size budget validation and cache eviction calculations.
    ///
    /// Falls back to the documented 256-byte budget estimate if serialization
    /// fails (which should be extremely rare in practice).
    #[must_use]
    pub fn serialized_size(&self) -> usize {
        const BUDGET_FALLBACK: usize = 256;

        match postcard::to_allocvec(self) {
            Ok(bytes) => bytes.len(),
            Err(e) => {
                log::error!(
                    "Failed to serialize GraphNodeSummary for size calculation: {e}. Falling back to {BUDGET_FALLBACK} byte budget estimate."
                );
                BUDGET_FALLBACK
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_serialized_size_budget() {
        let summary = GraphNodeSummary::new(
            Arc::from("test_function"),
            NodeKind::Function,
            Arc::from(Path::new("src/lib.rs")),
            10,
            0,
            12,
            0,
        );
        assert!(summary.serialized_size() <= 256);
    }

    #[test]
    fn test_roundtrip_serialization() {
        let summary = GraphNodeSummary::new(
            Arc::from("test_fn"),
            NodeKind::Function,
            Arc::from(Path::new("src/lib.rs")),
            1,
            0,
            2,
            10,
        );

        let bytes = postcard::to_allocvec(&summary).unwrap();
        let restored: GraphNodeSummary = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(summary, restored);
    }

    #[test]
    fn test_new_fields_defaults() {
        let summary = GraphNodeSummary::new(
            "test",
            NodeKind::Function,
            Arc::from(PathBuf::from("src/lib.rs").as_path()),
            3,
            1,
            4,
            2,
        );

        assert!(summary.qualified_name.is_none());
        assert!(summary.visibility.is_none());
        assert!(summary.signature.is_none());
        assert!(!summary.is_async);
        assert!(!summary.is_static);
    }
}
