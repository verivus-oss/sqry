//! Compressed Sparse Row (CSR) format for edge storage.
//!
//! This module implements `CsrGraph`, a read-optimized edge storage format
//! that provides O(1) edge range lookup per node.
//!
//! # Design
//!
//! CSR stores edges in a compact format:
//! - `row_ptr[i]` points to the first edge of node `i` in `col_idx`
//! - `row_ptr[i+1] - row_ptr[i]` is the out-degree of node `i`
//! - `col_idx[row_ptr[i]..row_ptr[i+1]]` are the targets of node `i`
//!
//! # Invariants
//!
//! The CSR format must satisfy these invariants:
//! 1. `row_ptr.len() == node_count + 1`
//! 2. `row_ptr` is monotonically non-decreasing
//! 3. `row_ptr[0] == 0`
//! 4. `row_ptr[last] == col_idx.len()`
//! 5. `col_idx.len() == edge_kind.len() == edge_seq.len()`
//! 6. All values in `col_idx` are < `node_count`
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::storage::csr::{CsrGraph, CsrBuilder};
//!
//! let mut builder = CsrBuilder::new(3);  // 3 nodes
//! builder.add_edge(0, 1, EdgeKind::Calls { argument_count: 0, is_async: false }, 1);
//! builder.add_edge(0, 2, EdgeKind::References, 2);
//! builder.add_edge(1, 2, EdgeKind::Calls { argument_count: 0, is_async: false }, 3);
//!
//! let csr = builder.build()?;
//! assert!(csr.validate().is_ok());
//!
//! // O(1) edge range lookup
//! let edges: Vec<_> = csr.edges_of(0).collect();
//! assert_eq!(edges.len(), 2);  // node 0 has 2 outgoing edges
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::graph::node::Span;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;

/// Compressed Sparse Row graph for read-optimized edge storage.
///
/// `CsrGraph` provides O(1) access to the edge range of any node.
/// It is immutable once constructed - use `CsrBuilder` to create instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CsrGraph {
    /// Number of nodes in the graph (determines valid node index range).
    node_count: usize,
    /// Row pointers: `row_ptr[i]` is the start index of node `i`'s edges.
    /// Length is `node_count + 1`.
    row_ptr: Vec<u32>,
    /// Column indices: target node IDs for each edge.
    col_idx: Vec<NodeId>,
    /// Edge kinds for each edge.
    edge_kind: Vec<EdgeKind>,
    /// Sequence numbers for merge ordering (last-writer-wins).
    edge_seq: Vec<u64>,
    /// Source spans for each edge (call-site locations for LSP call hierarchy).
    /// Multiple spans are stored when the same edge (source, target, kind) has
    /// multiple call sites (e.g., function A calls function B at lines 10, 20, 30).
    edge_spans: Vec<Vec<Span>>,
}

/// Error type for CSR validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(missing_docs)] // Error variant fields are self-explanatory
pub enum CsrError {
    /// `row_ptr.len()` != `node_count` + 1
    RowPtrLengthMismatch { expected: usize, actual: usize },
    /// `row_ptr` is not monotonically non-decreasing
    NonMonotonicRowPtr {
        index: usize,
        previous: u32,
        current: u32,
    },
    /// `row_ptr[0] != 0`
    RowPtrNotStartingAtZero { first: u32 },
    /// `row_ptr[last] != col_idx.len()`
    RowPtrColIdxMismatch {
        row_ptr_last: u32,
        col_idx_len: usize,
    },
    /// Arrays have different lengths
    ArrayLengthMismatch {
        col_idx_len: usize,
        edge_kind_len: usize,
        edge_seq_len: usize,
        edge_spans_len: usize,
    },
    /// Column index out of bounds
    ColIdxOutOfBounds {
        edge_index: usize,
        col_value: u32,
        node_count: usize,
    },
    /// Builder error: edge source out of bounds
    SourceOutOfBounds { source: u32, node_count: usize },
    /// Builder error: edge target out of bounds
    TargetOutOfBounds { target: u32, node_count: usize },
}

impl std::error::Error for CsrError {}

impl fmt::Display for CsrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RowPtrLengthMismatch { expected, actual } => {
                write!(
                    f,
                    "row_ptr length mismatch: expected {expected}, got {actual}"
                )
            }
            Self::NonMonotonicRowPtr {
                index,
                previous,
                current,
            } => {
                write!(
                    f,
                    "row_ptr not monotonic at index {index}: {previous} > {current}"
                )
            }
            Self::RowPtrNotStartingAtZero { first } => {
                write!(f, "row_ptr[0] must be 0, got {first}")
            }
            Self::RowPtrColIdxMismatch {
                row_ptr_last,
                col_idx_len,
            } => {
                write!(
                    f,
                    "row_ptr[last] ({row_ptr_last}) != col_idx.len() ({col_idx_len})"
                )
            }
            Self::ArrayLengthMismatch {
                col_idx_len,
                edge_kind_len,
                edge_seq_len,
                edge_spans_len,
            } => {
                write!(
                    f,
                    "array length mismatch: col_idx={col_idx_len}, edge_kind={edge_kind_len}, edge_seq={edge_seq_len}, edge_spans={edge_spans_len}"
                )
            }
            Self::ColIdxOutOfBounds {
                edge_index,
                col_value,
                node_count,
            } => {
                write!(
                    f,
                    "col_idx[{edge_index}] = {col_value} out of bounds (node_count = {node_count})"
                )
            }
            Self::SourceOutOfBounds { source, node_count } => {
                write!(
                    f,
                    "edge source {source} out of bounds (node_count = {node_count})"
                )
            }
            Self::TargetOutOfBounds { target, node_count } => {
                write!(
                    f,
                    "edge target {target} out of bounds (node_count = {node_count})"
                )
            }
        }
    }
}

/// A single edge reference from the CSR graph.
#[derive(Debug, Clone, PartialEq)]
pub struct EdgeRef {
    /// Index of this edge in the CSR arrays.
    pub index: usize,
    /// Target node of this edge.
    pub target: NodeId,
    /// Kind of this edge.
    pub kind: EdgeKind,
    /// Sequence number for merge ordering.
    pub seq: u64,
    /// Source spans for this edge (call-site locations for LSP call hierarchy).
    /// Multiple spans when the same edge has multiple call sites.
    pub spans: Vec<Span>,
}

impl CsrGraph {
    /// Creates an empty CSR graph with the given node count.
    #[must_use]
    pub fn empty(node_count: usize) -> Self {
        Self {
            node_count,
            row_ptr: vec![0; node_count + 1],
            col_idx: Vec::new(),
            edge_kind: Vec::new(),
            edge_seq: Vec::new(),
            edge_spans: Vec::new(),
        }
    }

    /// Creates a CSR graph from raw components.
    ///
    /// # Safety
    ///
    /// This function does not validate the CSR invariants.
    /// Call `validate()` after construction to ensure correctness.
    #[must_use]
    pub fn from_raw(
        node_count: usize,
        row_ptr: Vec<u32>,
        col_idx: Vec<NodeId>,
        edge_kind: Vec<EdgeKind>,
        edge_seq: Vec<u64>,
        edge_spans: Vec<Vec<Span>>,
    ) -> Self {
        Self {
            node_count,
            row_ptr,
            col_idx,
            edge_kind,
            edge_seq,
            edge_spans,
        }
    }

    /// Returns the number of nodes.
    #[inline]
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// Returns the number of edges.
    #[inline]
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.col_idx.len()
    }

    /// Returns the out-degree of a node (number of outgoing edges).
    ///
    /// Returns 0 if the node index is out of bounds.
    #[must_use]
    pub fn out_degree(&self, node: u32) -> usize {
        let idx = node as usize;
        if idx >= self.node_count {
            return 0;
        }
        let start = self.row_ptr[idx] as usize;
        let end = self.row_ptr[idx + 1] as usize;
        end - start
    }

    /// Returns an iterator over the edges of a node.
    ///
    /// This is O(1) to get the range, then O(degree) to iterate.
    pub fn edges_of(&self, node: u32) -> impl Iterator<Item = EdgeRef> + '_ {
        let idx = node as usize;
        if idx >= self.node_count {
            return EdgeIter {
                csr: self,
                start: 0,
                end: 0,
            };
        }
        let start = self.row_ptr[idx] as usize;
        let end = self.row_ptr[idx + 1] as usize;
        EdgeIter {
            csr: self,
            start,
            end,
        }
    }

    /// Returns the edge at the given index.
    ///
    /// Returns `None` if the index is out of bounds.
    #[must_use]
    pub fn edge_at(&self, index: usize) -> Option<EdgeRef> {
        if index >= self.col_idx.len() {
            return None;
        }
        Some(EdgeRef {
            index,
            target: self.col_idx[index],
            kind: self.edge_kind[index].clone(),
            seq: self.edge_seq[index],
            spans: self.edge_spans.get(index).cloned().unwrap_or_default(),
        })
    }

    /// Validates all CSR invariants.
    ///
    /// Returns `Ok(())` if all invariants hold, or an error describing
    /// the first violation found.
    ///
    /// # Errors
    ///
    /// Returns `CsrError` when a CSR invariant is violated.
    pub fn validate(&self) -> Result<(), CsrError> {
        // Invariant 1: row_ptr length == node_count + 1
        let expected_len = self.node_count + 1;
        if self.row_ptr.len() != expected_len {
            return Err(CsrError::RowPtrLengthMismatch {
                expected: expected_len,
                actual: self.row_ptr.len(),
            });
        }

        // Invariant 3: row_ptr[0] == 0 (check before monotonicity to give better error)
        if !self.row_ptr.is_empty() && self.row_ptr[0] != 0 {
            return Err(CsrError::RowPtrNotStartingAtZero {
                first: self.row_ptr[0],
            });
        }

        // Invariant 2: row_ptr monotonically non-decreasing
        for i in 1..self.row_ptr.len() {
            if self.row_ptr[i] < self.row_ptr[i - 1] {
                return Err(CsrError::NonMonotonicRowPtr {
                    index: i,
                    previous: self.row_ptr[i - 1],
                    current: self.row_ptr[i],
                });
            }
        }

        // Invariant 4: row_ptr[last] == col_idx.len()
        if let Some(&last) = self.row_ptr.last()
            && last as usize != self.col_idx.len()
        {
            return Err(CsrError::RowPtrColIdxMismatch {
                row_ptr_last: last,
                col_idx_len: self.col_idx.len(),
            });
        }

        // Invariant 5: col_idx, edge_kind, edge_seq, edge_spans same length
        if self.col_idx.len() != self.edge_kind.len()
            || self.col_idx.len() != self.edge_seq.len()
            || self.col_idx.len() != self.edge_spans.len()
        {
            return Err(CsrError::ArrayLengthMismatch {
                col_idx_len: self.col_idx.len(),
                edge_kind_len: self.edge_kind.len(),
                edge_seq_len: self.edge_seq.len(),
                edge_spans_len: self.edge_spans.len(),
            });
        }

        // Invariant 6: col_idx bounds check
        let node_count_u32 = u32::try_from(self.node_count).unwrap_or(u32::MAX);
        for (i, &col) in self.col_idx.iter().enumerate() {
            if col.index() >= node_count_u32 {
                return Err(CsrError::ColIdxOutOfBounds {
                    edge_index: i,
                    col_value: col.index(),
                    node_count: self.node_count,
                });
            }
        }

        Ok(())
    }

    /// Returns a reference to the row pointers array.
    #[must_use]
    pub fn row_ptr(&self) -> &[u32] {
        &self.row_ptr
    }

    /// Returns a reference to the column indices array.
    #[must_use]
    pub fn col_idx(&self) -> &[NodeId] {
        &self.col_idx
    }

    /// Returns a reference to the edge kinds array.
    #[must_use]
    pub fn edge_kind(&self) -> &[EdgeKind] {
        &self.edge_kind
    }

    /// Returns a reference to the sequence numbers array.
    #[must_use]
    pub fn edge_seq(&self) -> &[u64] {
        &self.edge_seq
    }

    /// Returns a reference to the edge spans array.
    /// Each edge has a vector of spans (multiple call sites per edge).
    #[must_use]
    pub fn edge_spans(&self) -> &[Vec<Span>] {
        &self.edge_spans
    }
}

impl Default for CsrGraph {
    fn default() -> Self {
        Self::empty(0)
    }
}

impl fmt::Display for CsrGraph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CsrGraph(nodes={}, edges={})",
            self.node_count,
            self.edge_count()
        )
    }
}

/// Iterator over edges of a node in CSR format.
#[derive(Debug)]
struct EdgeIter<'a> {
    csr: &'a CsrGraph,
    start: usize,
    end: usize,
}

impl Iterator for EdgeIter<'_> {
    type Item = EdgeRef;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start >= self.end {
            return None;
        }
        let idx = self.start;
        self.start += 1;
        Some(EdgeRef {
            index: idx,
            target: self.csr.col_idx[idx],
            kind: self.csr.edge_kind[idx].clone(),
            seq: self.csr.edge_seq[idx],
            spans: self.csr.edge_spans.get(idx).cloned().unwrap_or_default(),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.start;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for EdgeIter<'_> {}

/// Builder for constructing CSR graphs efficiently.
///
/// Edges are added in any order, then sorted and compacted during `build()`.
#[derive(Debug, Clone)]
pub struct CsrBuilder {
    node_count: usize,
    /// Edges: (source, target, kind, seq, spans)
    /// Spans is a Vec to support multiple call sites per edge.
    edges: Vec<(u32, NodeId, EdgeKind, u64, Vec<Span>)>,
}

impl CsrBuilder {
    /// Creates a new builder for a graph with the given node count.
    #[must_use]
    pub fn new(node_count: usize) -> Self {
        Self {
            node_count,
            edges: Vec::new(),
        }
    }

    /// Creates a builder with pre-allocated edge capacity.
    #[must_use]
    pub fn with_capacity(node_count: usize, edge_capacity: usize) -> Self {
        Self {
            node_count,
            edges: Vec::with_capacity(edge_capacity),
        }
    }

    /// Adds an edge to the builder.
    ///
    /// # Errors
    ///
    /// Returns an error if source or target is out of bounds.
    pub fn add_edge(
        &mut self,
        source: u32,
        target: NodeId,
        kind: EdgeKind,
        seq: u64,
        spans: Vec<Span>,
    ) -> Result<(), CsrError> {
        if source as usize >= self.node_count {
            return Err(CsrError::SourceOutOfBounds {
                source,
                node_count: self.node_count,
            });
        }
        if target.index() as usize >= self.node_count {
            return Err(CsrError::TargetOutOfBounds {
                target: target.index(),
                node_count: self.node_count,
            });
        }
        self.edges.push((source, target, kind, seq, spans));
        Ok(())
    }

    /// Adds an edge without bounds checking.
    ///
    /// # Safety
    ///
    /// Caller must ensure source and target are < `node_count`.
    pub fn add_edge_unchecked(
        &mut self,
        source: u32,
        target: NodeId,
        kind: EdgeKind,
        seq: u64,
        spans: Vec<Span>,
    ) {
        self.edges.push((source, target, kind, seq, spans));
    }

    /// Returns the current number of edges added.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Builds the CSR graph.
    ///
    /// This sorts edges by source node and constructs the CSR arrays.
    /// The resulting graph is validated before returning.
    ///
    /// # Errors
    ///
    /// Returns an error if validation fails (should not happen with
    /// properly bounded edges).
    pub fn build(mut self) -> Result<CsrGraph, CsrError> {
        // Sort edges by source node (stable sort preserves insertion order for same source)
        self.edges.sort_by_key(|(src, _, _, _, _)| *src);

        // Build CSR arrays
        let mut row_ptr = vec![0u32; self.node_count + 1];
        let mut col_idx = Vec::with_capacity(self.edges.len());
        let mut edge_kind = Vec::with_capacity(self.edges.len());
        let mut edge_seq = Vec::with_capacity(self.edges.len());
        let mut edge_spans = Vec::with_capacity(self.edges.len());

        // Count edges per node
        for &(src, _, _, _, _) in &self.edges {
            row_ptr[src as usize + 1] += 1;
        }

        // Prefix sum to get row pointers
        for i in 1..row_ptr.len() {
            row_ptr[i] += row_ptr[i - 1];
        }

        // Fill edge arrays (already sorted)
        for (_, target, kind, seq, spans) in self.edges {
            col_idx.push(target);
            edge_kind.push(kind);
            edge_seq.push(seq);
            edge_spans.push(spans);
        }

        let csr = CsrGraph::from_raw(
            self.node_count,
            row_ptr,
            col_idx,
            edge_kind,
            edge_seq,
            edge_spans,
        );
        csr.validate()?;
        Ok(csr)
    }

    /// Builds the CSR graph without validation.
    ///
    /// # Safety
    ///
    /// Call `validate()` on the result to ensure correctness.
    #[must_use]
    pub fn build_unchecked(mut self) -> CsrGraph {
        // Sort edges by source node
        self.edges.sort_by_key(|(src, _, _, _, _)| *src);

        // Build CSR arrays
        let mut row_ptr = vec![0u32; self.node_count + 1];
        let mut col_idx = Vec::with_capacity(self.edges.len());
        let mut edge_kind = Vec::with_capacity(self.edges.len());
        let mut edge_seq = Vec::with_capacity(self.edges.len());
        let mut edge_spans = Vec::with_capacity(self.edges.len());

        // Count edges per node
        for &(src, _, _, _, _) in &self.edges {
            row_ptr[src as usize + 1] += 1;
        }

        // Prefix sum
        for i in 1..row_ptr.len() {
            row_ptr[i] += row_ptr[i - 1];
        }

        // Fill edge arrays
        for (_, target, kind, seq, spans) in self.edges {
            col_idx.push(target);
            edge_kind.push(kind);
            edge_seq.push(seq);
            edge_spans.push(spans);
        }

        CsrGraph::from_raw(
            self.node_count,
            row_ptr,
            col_idx,
            edge_kind,
            edge_seq,
            edge_spans,
        )
    }
}

/// Statistics about a CSR graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsrStats {
    /// Number of nodes.
    pub node_count: usize,
    /// Number of edges.
    pub edge_count: usize,
    /// Memory usage of `row_ptr` (bytes).
    pub row_ptr_bytes: usize,
    /// Memory usage of `col_idx` (bytes).
    pub col_idx_bytes: usize,
    /// Memory usage of `edge_kind` (approximate bytes).
    pub edge_kind_bytes: usize,
    /// Memory usage of `edge_seq` (bytes).
    pub edge_seq_bytes: usize,
    /// Memory usage of `edge_spans` (bytes).
    /// Includes Vec overhead per edge plus Span data.
    pub edge_spans_bytes: usize,
    /// Total number of spans across all edges.
    pub total_span_count: usize,
}

impl CsrGraph {
    /// Returns statistics about the CSR graph.
    #[must_use]
    pub fn stats(&self) -> CsrStats {
        // Calculate span memory: Vec overhead per edge + actual span data
        let vec_overhead = self.edge_spans.len() * std::mem::size_of::<Vec<Span>>();
        let total_span_count: usize = self.edge_spans.iter().map(Vec::len).sum();
        let span_data_bytes = total_span_count * std::mem::size_of::<Span>();

        CsrStats {
            node_count: self.node_count,
            edge_count: self.col_idx.len(),
            row_ptr_bytes: self.row_ptr.len() * std::mem::size_of::<u32>(),
            col_idx_bytes: self.col_idx.len() * std::mem::size_of::<NodeId>(),
            edge_kind_bytes: self.edge_kind.len() * std::mem::size_of::<EdgeKind>(),
            edge_seq_bytes: self.edge_seq.len() * std::mem::size_of::<u64>(),
            edge_spans_bytes: vec_overhead + span_data_bytes,
            total_span_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_id(index: u32) -> NodeId {
        NodeId::new(index, 1)
    }

    #[test]
    fn test_empty_graph() {
        let csr = CsrGraph::empty(5);
        assert_eq!(csr.node_count(), 5);
        assert_eq!(csr.edge_count(), 0);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_empty_graph_zero_nodes() {
        let csr = CsrGraph::empty(0);
        assert_eq!(csr.node_count(), 0);
        assert_eq!(csr.edge_count(), 0);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_builder_simple() {
        let mut builder = CsrBuilder::new(3);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(0, node_id(2), EdgeKind::References, 2, vec![])
            .unwrap();
        builder
            .add_edge(
                1,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                3,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert_eq!(csr.node_count(), 3);
        assert_eq!(csr.edge_count(), 3);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_builder_out_of_order() {
        let mut builder = CsrBuilder::new(4);
        // Add edges out of order
        builder
            .add_edge(
                2,
                node_id(3),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                2,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                1,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                3,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                0,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                4,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert!(csr.validate().is_ok());

        // Check edges are sorted by source
        assert_eq!(csr.out_degree(0), 2);
        assert_eq!(csr.out_degree(1), 1);
        assert_eq!(csr.out_degree(2), 1);
        assert_eq!(csr.out_degree(3), 0);
    }

    #[test]
    fn test_builder_source_out_of_bounds() {
        let mut builder = CsrBuilder::new(3);
        let result = builder.add_edge(
            3,
            node_id(0),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            1,
            vec![],
        );
        assert!(matches!(result, Err(CsrError::SourceOutOfBounds { .. })));
    }

    #[test]
    fn test_builder_target_out_of_bounds() {
        let mut builder = CsrBuilder::new(3);
        let result = builder.add_edge(
            0,
            node_id(3),
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            },
            1,
            vec![],
        );
        assert!(matches!(result, Err(CsrError::TargetOutOfBounds { .. })));
    }

    #[test]
    fn test_edges_of() {
        let mut builder = CsrBuilder::new(3);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(0, node_id(2), EdgeKind::References, 2, vec![])
            .unwrap();

        let csr = builder.build().unwrap();

        let edges: Vec<_> = csr.edges_of(0).collect();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].target.index(), 1);
        assert_eq!(edges[1].target.index(), 2);
    }

    #[test]
    fn test_edges_of_empty_node() {
        let csr = CsrGraph::empty(3);
        let edges: Vec<_> = csr.edges_of(1).collect();
        assert!(edges.is_empty());
    }

    #[test]
    fn test_edges_of_out_of_bounds() {
        let csr = CsrGraph::empty(3);
        let edges: Vec<_> = csr.edges_of(10).collect();
        assert!(edges.is_empty());
    }

    #[test]
    fn test_out_degree() {
        let mut builder = CsrBuilder::new(4);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                0,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                2,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                0,
                node_id(3),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                3,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                2,
                node_id(3),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                4,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();

        assert_eq!(csr.out_degree(0), 3);
        assert_eq!(csr.out_degree(1), 0);
        assert_eq!(csr.out_degree(2), 1);
        assert_eq!(csr.out_degree(3), 0);
        assert_eq!(csr.out_degree(100), 0);
    }

    #[test]
    fn test_edge_at() {
        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                42,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();

        let edge = csr.edge_at(0).unwrap();
        assert_eq!(edge.index, 0);
        assert_eq!(edge.target.index(), 1);
        assert_eq!(edge.seq, 42);
        assert!(edge.spans.is_empty());

        assert!(csr.edge_at(1).is_none());
    }

    #[test]
    fn test_validate_row_ptr_length() {
        let csr = CsrGraph::from_raw(
            3,
            vec![0, 1], // Wrong length: should be 4
            vec![node_id(1)],
            vec![EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            }],
            vec![1],
            vec![vec![]],
        );
        let err = csr.validate().unwrap_err();
        assert!(matches!(err, CsrError::RowPtrLengthMismatch { .. }));
    }

    #[test]
    fn test_validate_row_ptr_not_starting_zero() {
        let csr = CsrGraph::from_raw(
            2,
            vec![1, 1, 1], // Should start at 0
            vec![],
            vec![],
            vec![],
            vec![],
        );
        let err = csr.validate().unwrap_err();
        assert!(matches!(err, CsrError::RowPtrNotStartingAtZero { .. }));
    }

    #[test]
    fn test_validate_non_monotonic() {
        let csr = CsrGraph::from_raw(
            3,
            vec![0, 2, 1, 2], // 2 > 1 is not monotonic
            vec![node_id(1), node_id(2)],
            vec![
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
            ],
            vec![1, 2],
            vec![vec![], vec![]],
        );
        let err = csr.validate().unwrap_err();
        assert!(matches!(err, CsrError::NonMonotonicRowPtr { .. }));
    }

    #[test]
    fn test_validate_row_ptr_col_idx_mismatch() {
        let csr = CsrGraph::from_raw(
            2,
            vec![0, 1, 3], // Last = 3, but col_idx.len() = 2
            vec![node_id(1), node_id(0)],
            vec![
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
            ],
            vec![1, 2],
            vec![vec![], vec![]],
        );
        let err = csr.validate().unwrap_err();
        assert!(matches!(err, CsrError::RowPtrColIdxMismatch { .. }));
    }

    #[test]
    fn test_validate_array_length_mismatch() {
        let csr = CsrGraph::from_raw(
            2,
            vec![0, 1, 2],
            vec![node_id(1), node_id(0)],
            vec![EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            }], // Wrong: only 1 element
            vec![1, 2],
            vec![vec![], vec![]],
        );
        let err = csr.validate().unwrap_err();
        assert!(matches!(err, CsrError::ArrayLengthMismatch { .. }));
    }

    #[test]
    fn test_validate_col_idx_out_of_bounds() {
        let csr = CsrGraph::from_raw(
            2, // node_count = 2
            vec![0, 1, 1],
            vec![node_id(5)], // 5 >= 2
            vec![EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
            }],
            vec![1],
            vec![vec![]],
        );
        let err = csr.validate().unwrap_err();
        assert!(matches!(err, CsrError::ColIdxOutOfBounds { .. }));
    }

    #[test]
    fn test_stats() {
        let mut builder = CsrBuilder::new(10);
        for i in 0..5 {
            builder
                .add_edge(
                    i,
                    node_id(i + 1),
                    EdgeKind::Calls {
                        argument_count: 0,
                        is_async: false,
                    },
                    u64::from(i),
                    vec![],
                )
                .unwrap();
        }
        let csr = builder.build().unwrap();

        let stats = csr.stats();
        assert_eq!(stats.node_count, 10);
        assert_eq!(stats.edge_count, 5);
        assert!(stats.row_ptr_bytes > 0);
        assert!(stats.col_idx_bytes > 0);
        // Note: edge_spans_bytes can be 0 if no spans are stored, so we don't assert on it
    }

    #[test]
    fn test_display() {
        let csr = CsrGraph::empty(10);
        let display = format!("{csr}");
        assert!(display.contains("nodes=10"));
        assert!(display.contains("edges=0"));
    }

    #[test]
    fn test_default() {
        let csr: CsrGraph = CsrGraph::default();
        assert_eq!(csr.node_count(), 0);
        assert!(csr.validate().is_ok());
    }

    #[test]
    fn test_exact_size_iterator() {
        let mut builder = CsrBuilder::new(3);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                0,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                2,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        let iter = csr.edges_of(0);

        // Use size_hint() since edges_of returns impl Iterator
        assert_eq!(iter.size_hint(), (2, Some(2)));
    }

    #[test]
    fn test_builder_with_capacity() {
        let builder = CsrBuilder::with_capacity(100, 1000);
        assert_eq!(builder.edge_count(), 0);
    }

    #[test]
    fn test_csr_error_display() {
        let err = CsrError::RowPtrLengthMismatch {
            expected: 10,
            actual: 5,
        };
        let display = format!("{err}");
        assert!(display.contains("10"));
        assert!(display.contains('5'));
    }

    #[test]
    fn test_single_node_with_edges() {
        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(0, node_id(1), EdgeKind::References, 2, vec![])
            .unwrap();
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Imports {
                    alias: None,
                    is_wildcard: false,
                },
                3,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert!(csr.validate().is_ok());
        assert_eq!(csr.out_degree(0), 3);
        assert_eq!(csr.out_degree(1), 0);
    }

    #[test]
    fn test_all_nodes_have_edges() {
        let mut builder = CsrBuilder::new(3);
        // Create a cycle: 0 -> 1 -> 2 -> 0
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                1,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                2,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                2,
                node_id(0),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                3,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert!(csr.validate().is_ok());
        assert_eq!(csr.out_degree(0), 1);
        assert_eq!(csr.out_degree(1), 1);
        assert_eq!(csr.out_degree(2), 1);
    }

    #[test]
    fn test_self_loop() {
        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(
                0,
                node_id(0),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert!(csr.validate().is_ok());

        let edges: Vec<_> = csr.edges_of(0).collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target.index(), 0);
    }

    #[test]
    fn test_row_ptr_and_col_idx_accessors() {
        let mut builder = CsrBuilder::new(3);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                1,
                vec![],
            )
            .unwrap();
        builder
            .add_edge(
                1,
                node_id(2),
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                },
                2,
                vec![],
            )
            .unwrap();

        let csr = builder.build().unwrap();

        assert_eq!(csr.row_ptr().len(), 4);
        assert_eq!(csr.col_idx().len(), 2);
        assert_eq!(csr.edge_kind().len(), 2);
        assert_eq!(csr.edge_seq().len(), 2);
        assert_eq!(csr.edge_spans().len(), 2);
    }

    #[test]
    fn test_edge_with_span() {
        use crate::graph::node::{Position, Span};

        let span = Span {
            start: Position {
                line: 10,
                column: 5,
            },
            end: Position {
                line: 10,
                column: 20,
            },
        };

        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 2,
                    is_async: false,
                },
                1,
                vec![span],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert!(csr.validate().is_ok());

        let edge = csr.edge_at(0).unwrap();
        assert_eq!(edge.spans.len(), 1);
        let edge_span = &edge.spans[0];
        assert_eq!(edge_span.start.line, 10);
        assert_eq!(edge_span.start.column, 5);
        assert_eq!(edge_span.end.line, 10);
        assert_eq!(edge_span.end.column, 20);
    }

    #[test]
    fn test_edge_with_multiple_spans() {
        use crate::graph::node::{Position, Span};

        let span1 = Span {
            start: Position {
                line: 10,
                column: 5,
            },
            end: Position {
                line: 10,
                column: 20,
            },
        };
        let span2 = Span {
            start: Position {
                line: 25,
                column: 8,
            },
            end: Position {
                line: 25,
                column: 30,
            },
        };

        let mut builder = CsrBuilder::new(2);
        builder
            .add_edge(
                0,
                node_id(1),
                EdgeKind::Calls {
                    argument_count: 2,
                    is_async: false,
                },
                1,
                vec![span1, span2],
            )
            .unwrap();

        let csr = builder.build().unwrap();
        assert!(csr.validate().is_ok());

        let edge = csr.edge_at(0).unwrap();
        assert_eq!(edge.spans.len(), 2);
        assert_eq!(edge.spans[0].start.line, 10);
        assert_eq!(edge.spans[1].start.line, 25);

        // Check stats account for spans
        let stats = csr.stats();
        assert_eq!(stats.total_span_count, 2);
    }
}
