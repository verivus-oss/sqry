//! Pass 4: Cross-file Linking - Create edges between files.
//!
//! This module implements the fourth pass of the build pipeline:
//! creating IMPORTS edges and cross-file CALLS/REFERENCES edges.
//!
//! # Overview
//!
//! Pass 4 handles relationships that span multiple files:
//! - IMPORTS edges (file A imports symbol from file B)
//! - Cross-file CALLS (function in A calls function in B)
//! - Cross-file REFERENCES (type in A references type in B)
//!
//! # `ExportMap`
//!
//! The [`ExportMap`] tracks exported symbols across the entire codebase,
//! enabling resolution of cross-file references. It maps qualified names
//! to their defining file and node.
//!
//! # Algorithm
//!
//! 1. Build `ExportMap` from all files' symbols
//! 2. For each file's unresolved references (from Pass 3):
//!    - Look up target in `ExportMap`
//!    - If found, create cross-file edge
//! 3. Process explicit imports:
//!    - Create IMPORTS edges from file node to imported symbol

use std::collections::HashMap;

use super::super::edge::EdgeKind;
#[cfg(test)]
use super::super::edge::ResolvedVia;
use super::super::file::FileId;
use super::super::node::NodeId;
use super::super::string::StringId;
use super::pass3_intra::{PendingEdge, UnresolvedRef};

/// Map of exported symbols for cross-file resolution.
///
/// The `ExportMap` tracks all symbols that can be referenced from other files,
/// mapping their qualified names to their location (file + node).
///
/// # Multiple Definitions
///
/// A qualified name may have multiple definitions across files (e.g., a real
/// definition in one file and a stub/forward declaration in another). The
/// `lookup_cross_file` method supports finding a definition in a different
/// file than the caller, which is essential for cross-file resolution.
#[derive(Debug, Clone, Default)]
pub struct ExportMap {
    /// Map from qualified name to list of (`file_id`, `node_id`) pairs.
    ///
    /// Multiple entries per name support cross-file resolution when stubs exist.
    exports: HashMap<String, Vec<(FileId, NodeId)>>,
}

impl ExportMap {
    /// Create a new empty `ExportMap`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an exported symbol.
    ///
    /// Multiple registrations for the same qualified name are allowed,
    /// supporting scenarios where stubs exist alongside real definitions.
    ///
    /// # Arguments
    ///
    /// * `qualified_name` - Fully qualified name of the symbol
    /// * `file_id` - File containing the symbol
    /// * `node_id` - Node representing the symbol
    pub fn register(&mut self, qualified_name: String, file_id: FileId, node_id: NodeId) {
        self.exports
            .entry(qualified_name)
            .or_default()
            .push((file_id, node_id));
    }

    /// Look up an exported symbol by qualified name.
    ///
    /// Returns the first registered file and node ID if found.
    /// For cross-file resolution, prefer `lookup_cross_file` instead.
    #[must_use]
    pub fn lookup(&self, qualified_name: &str) -> Option<(FileId, NodeId)> {
        self.exports
            .get(qualified_name)
            .and_then(|entries| entries.first().copied())
    }

    /// Look up an exported symbol, preferring definitions in a different file.
    ///
    /// This is the primary method for cross-file resolution. When a symbol has
    /// multiple definitions (e.g., a stub in the caller's file and a real
    /// definition elsewhere), this returns the definition in a different file.
    ///
    /// # Arguments
    ///
    /// * `qualified_name` - The qualified name to look up
    /// * `exclude_file` - The file to exclude from results (typically the caller's file)
    ///
    /// # Returns
    ///
    /// The first matching (`FileId`, `NodeId`) in a file other than `exclude_file`,
    /// or `None` if no cross-file definition exists.
    #[must_use]
    pub fn lookup_cross_file(
        &self,
        qualified_name: &str,
        exclude_file: FileId,
    ) -> Option<(FileId, NodeId)> {
        self.exports.get(qualified_name).and_then(|entries| {
            entries
                .iter()
                .find(|(fid, _)| *fid != exclude_file)
                .copied()
        })
    }

    /// Check if a symbol is exported.
    #[must_use]
    pub fn contains(&self, qualified_name: &str) -> bool {
        self.exports.contains_key(qualified_name)
    }

    /// Get the number of unique exported symbol names.
    #[must_use]
    pub fn len(&self) -> usize {
        self.exports.len()
    }

    /// Check if the export map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exports.is_empty()
    }

    /// Remove all exports from a specific file.
    ///
    /// Used during incremental updates when a file is removed or re-indexed.
    pub fn remove_file(&mut self, file_id: FileId) {
        for entries in self.exports.values_mut() {
            entries.retain(|(fid, _)| *fid != file_id);
        }
        // Remove empty entries
        self.exports.retain(|_, entries| !entries.is_empty());
    }

    /// Iterate over all exports.
    ///
    /// Returns the first entry for each qualified name for backward compatibility.
    pub fn iter(&self) -> impl Iterator<Item = (&String, (FileId, NodeId))> + '_ {
        self.exports
            .iter()
            .filter_map(|(name, entries)| entries.first().map(|e| (name, *e)))
    }
}

/// Statistics from Pass 4 execution.
#[derive(Debug, Clone, Default)]
pub struct Pass4Stats {
    /// Number of unresolved references processed.
    pub unresolved_processed: usize,
    /// Number of cross-file CALLS edges created.
    pub cross_file_calls: usize,
    /// Number of cross-file REFERENCES edges created.
    pub cross_file_references: usize,
    /// Number of IMPORTS edges created.
    pub imports_created: usize,
    /// Number of references that remained unresolved.
    pub still_unresolved: usize,
}

/// Import declaration from a source file.
#[derive(Debug, Clone)]
pub struct ImportDecl {
    /// The qualified name being imported.
    pub imported_name: String,
    /// Optional alias name (as written in source, e.g., `import foo as bar` → "bar").
    ///
    /// Used for resolving `UnresolvedRef.target_name` that refers to the alias.
    pub alias_name: Option<String>,
    /// Optional alias `StringId` for `EdgeKind::Imports { alias }`.
    ///
    /// This MUST be a global (interner) `StringId` by the time Pass 4 runs.
    pub alias_id: Option<StringId>,
    /// Whether this import is a wildcard import (`import *` / `from x import *`).
    pub is_wildcard: bool,
    /// Line number of the import statement.
    pub line: usize,
}

// Note: UnresolvedRef is imported from pass3_intra.
// It carries `kind: EdgeKind` which preserves call metadata (argument_count, is_async)
// through the Pass 3 -> Pass 4 boundary.

/// Create cross-file edges using the `ExportMap`.
///
/// Resolves unresolved references from Pass 3 by looking up targets
/// in the `ExportMap` and creating appropriate cross-file edges.
///
/// # Arguments
///
/// * `unresolved` - Unresolved references from Pass 3
/// * `imports` - Import declarations from the source file
/// * `export_map` - Global export map for symbol resolution
/// * `file_node` - Node representing the source file (for IMPORTS edges)
/// * `source_file` - `FileId` of the source file
///
/// # Returns
///
/// Tuple of (stats, pending edges to add).
#[must_use]
pub fn pass4_cross_file(
    unresolved: &[UnresolvedRef],
    imports: &[ImportDecl],
    export_map: &ExportMap,
    file_node: Option<NodeId>,
    source_file: FileId,
) -> (Pass4Stats, Vec<PendingEdge>) {
    let mut stats = Pass4Stats::default();
    let mut edges = Vec::new();

    let alias_map = build_alias_map(imports);

    process_unresolved_refs(
        unresolved,
        export_map,
        &alias_map,
        source_file,
        &mut stats,
        &mut edges,
    );
    process_import_edges(
        imports,
        export_map,
        file_node,
        source_file,
        &mut stats,
        &mut edges,
    );

    (stats, edges)
}

fn build_alias_map(imports: &[ImportDecl]) -> HashMap<&str, &str> {
    let mut alias_map: HashMap<&str, &str> = HashMap::new();
    for import in imports {
        if let Some(alias_name) = &import.alias_name {
            alias_map.insert(alias_name.as_str(), import.imported_name.as_str());
        }
    }
    alias_map
}

fn process_unresolved_refs(
    unresolved: &[UnresolvedRef],
    export_map: &ExportMap,
    alias_map: &HashMap<&str, &str>,
    source_file: FileId,
    stats: &mut Pass4Stats,
    edges: &mut Vec<PendingEdge>,
) {
    for unref in unresolved {
        stats.unresolved_processed += 1;
        if let Some(edge) = resolve_unresolved_ref(unref, export_map, alias_map, source_file, stats)
        {
            edges.push(edge);
        }
    }
}

fn process_import_edges(
    imports: &[ImportDecl],
    export_map: &ExportMap,
    file_node: Option<NodeId>,
    source_file: FileId,
    stats: &mut Pass4Stats,
    edges: &mut Vec<PendingEdge>,
) {
    let Some(file_node_id) = file_node else {
        return;
    };

    for import in imports {
        if let Some((_target_file, target_node)) = export_map.lookup(&import.imported_name) {
            stats.imports_created += 1;
            edges.push(PendingEdge {
                source: file_node_id,
                target: target_node,
                kind: EdgeKind::Imports {
                    alias: import.alias_id,
                    is_wildcard: import.is_wildcard,
                },
                file: source_file,
                spans: vec![], // Cross-file import edges don't have span info
            });
        }
    }
}

fn resolve_unresolved_ref(
    unref: &UnresolvedRef,
    export_map: &ExportMap,
    alias_map: &HashMap<&str, &str>,
    source_file: FileId,
    stats: &mut Pass4Stats,
) -> Option<PendingEdge> {
    let target_name = alias_map
        .get(unref.target_name.as_str())
        .copied()
        .unwrap_or(unref.target_name.as_str());

    // Use lookup_cross_file to find a definition in a different file.
    // This handles cases where a stub exists in the source file but the real
    // definition is elsewhere.
    let Some((_target_file, target_node)) = export_map.lookup_cross_file(target_name, source_file)
    else {
        stats.still_unresolved += 1;
        return None;
    };

    update_stats_for_edge_kind(stats, &unref.kind);

    // Create span only if explicit location was provided
    let spans = if unref.has_location {
        vec![crate::graph::node::Span {
            start: crate::graph::node::Position {
                line: unref.line,
                column: unref.column,
            },
            end: crate::graph::node::Position {
                line: unref.line,
                column: unref.column,
            },
        }]
    } else {
        vec![]
    };

    Some(PendingEdge {
        source: unref.source_node,
        target: target_node,
        kind: unref.kind.clone(),
        file: source_file,
        spans,
    })
}

fn update_stats_for_edge_kind(stats: &mut Pass4Stats, kind: &EdgeKind) {
    match kind {
        EdgeKind::Calls { .. } => {
            stats.cross_file_calls += 1;
        }
        _ => {
            stats.cross_file_references += 1;
        }
    }
}

/// Build an `ExportMap` from multiple files' symbol→node mappings.
///
/// # Arguments
///
/// * `file_symbols` - Iterator of (`file_id`, `symbol_to_node` mapping) pairs
///
/// # Returns
///
/// A populated `ExportMap`.
pub fn build_export_map<'a>(
    file_symbols: impl Iterator<Item = (FileId, &'a HashMap<String, NodeId>)>,
) -> ExportMap {
    let mut export_map = ExportMap::new();

    for (file_id, symbol_to_node) in file_symbols {
        for (qualified_name, &node_id) in symbol_to_node {
            export_map.register(qualified_name.clone(), file_id, node_id);
        }
    }

    export_map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_map_register_and_lookup() {
        let mut export_map = ExportMap::new();
        let file_id = FileId::new(0);
        let node_id = NodeId::new(1, 1);

        export_map.register("module::MyClass".to_string(), file_id, node_id);

        assert!(export_map.contains("module::MyClass"));
        assert!(!export_map.contains("other::Class"));

        let result = export_map.lookup("module::MyClass");
        assert_eq!(result, Some((file_id, node_id)));
    }

    #[test]
    fn test_export_map_remove_file() {
        let mut export_map = ExportMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);

        export_map.register("a::Foo".to_string(), file_a, NodeId::new(0, 1));
        export_map.register("a::Bar".to_string(), file_a, NodeId::new(1, 1));
        export_map.register("b::Baz".to_string(), file_b, NodeId::new(2, 1));

        assert_eq!(export_map.len(), 3);

        export_map.remove_file(file_a);

        assert_eq!(export_map.len(), 1);
        assert!(!export_map.contains("a::Foo"));
        assert!(!export_map.contains("a::Bar"));
        assert!(export_map.contains("b::Baz"));
    }

    #[test]
    fn test_pass4_resolves_cross_file_call() {
        let mut export_map = ExportMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let source_node = NodeId::new(0, 1);
        let target_node = NodeId::new(1, 1);

        // Register target in file B
        export_map.register("module_b::helper".to_string(), file_b, target_node);

        // Unresolved reference from file A with default/unknown call metadata
        let unresolved = vec![UnresolvedRef {
            source_node,
            target_name: "module_b::helper".to_string(),
            kind: EdgeKind::Calls {
                argument_count: 255,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            source_file: file_a,
            line: 0,
            column: 0,
            has_location: false,
        }];

        let (stats, edges) = pass4_cross_file(&unresolved, &[], &export_map, None, file_a);

        assert_eq!(stats.unresolved_processed, 1);
        assert_eq!(stats.cross_file_calls, 1);
        assert_eq!(stats.still_unresolved, 0);
        assert_eq!(edges.len(), 1);

        let edge = &edges[0];
        assert_eq!(edge.source, source_node);
        assert_eq!(edge.target, target_node);
        assert!(matches!(
            edge.kind,
            EdgeKind::Calls {
                argument_count: 255,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            }
        ));
    }

    #[test]
    fn test_pass4_preserves_call_metadata() {
        let mut export_map = ExportMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let source_node = NodeId::new(0, 1);
        let target_node = NodeId::new(1, 1);

        // Register target in file B
        export_map.register("module_b::async_helper".to_string(), file_b, target_node);

        // Unresolved reference from file A with async call and 3 arguments
        let unresolved = vec![UnresolvedRef {
            source_node,
            target_name: "module_b::async_helper".to_string(),
            kind: EdgeKind::Calls {
                argument_count: 3,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            },
            source_file: file_a,
            line: 10,
            column: 5,
            has_location: true,
        }];

        let (stats, edges) = pass4_cross_file(&unresolved, &[], &export_map, None, file_a);

        assert_eq!(stats.unresolved_processed, 1);
        assert_eq!(stats.cross_file_calls, 1);
        assert_eq!(edges.len(), 1);

        let edge = &edges[0];
        // Verify that call metadata (argument_count, is_async) is preserved
        assert!(matches!(
            edge.kind,
            EdgeKind::Calls {
                argument_count: 3,
                is_async: true,
                resolved_via: ResolvedVia::Direct,
            }
        ));
    }

    #[test]
    fn test_pass4_creates_imports_edge() {
        let mut export_map = ExportMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let file_node = NodeId::new(10, 1);
        let target_node = NodeId::new(1, 1);

        export_map.register("module_b::Widget".to_string(), file_b, target_node);

        let imports = vec![ImportDecl {
            imported_name: "module_b::Widget".to_string(),
            alias_name: None,
            alias_id: None,
            is_wildcard: false,
            line: 1,
        }];

        let (stats, edges) = pass4_cross_file(&[], &imports, &export_map, Some(file_node), file_a);

        assert_eq!(stats.imports_created, 1);
        assert_eq!(edges.len(), 1);

        let edge = &edges[0];
        assert_eq!(edge.source, file_node);
        assert_eq!(edge.target, target_node);
        assert!(matches!(
            edge.kind,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false
            }
        ));
    }

    #[test]
    fn test_pass4_import_edge_preserves_metadata() {
        let mut export_map = ExportMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let file_node = NodeId::new(10, 1);
        let target_node = NodeId::new(1, 1);

        export_map.register("module_b::Widget".to_string(), file_b, target_node);

        let alias_id = StringId::new(7);
        let imports = vec![ImportDecl {
            imported_name: "module_b::Widget".to_string(),
            alias_name: Some("W".to_string()),
            alias_id: Some(alias_id),
            is_wildcard: true,
            line: 1,
        }];

        let (stats, edges) = pass4_cross_file(&[], &imports, &export_map, Some(file_node), file_a);

        assert_eq!(stats.imports_created, 1);
        assert_eq!(edges.len(), 1);
        assert!(matches!(
            edges[0].kind,
            EdgeKind::Imports {
                alias: Some(id),
                is_wildcard: true,
            } if id == alias_id
        ));
    }

    #[test]
    fn test_pass4_resolves_alias() {
        let mut export_map = ExportMap::new();
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);
        let source_node = NodeId::new(0, 1);
        let target_node = NodeId::new(1, 1);

        export_map.register(
            "very::long::module::path::Helper".to_string(),
            file_b,
            target_node,
        );

        // Import with alias
        let imports = vec![ImportDecl {
            imported_name: "very::long::module::path::Helper".to_string(),
            alias_name: Some("Helper".to_string()),
            alias_id: Some(StringId::new(42)),
            is_wildcard: false,
            line: 1,
        }];

        // Reference uses the alias (type reference)
        let unresolved = vec![UnresolvedRef {
            source_node,
            target_name: "Helper".to_string(),
            kind: EdgeKind::References,
            source_file: file_a,
            line: 5,
            column: 10,
            has_location: true,
        }];

        let (stats, edges) = pass4_cross_file(&unresolved, &imports, &export_map, None, file_a);

        assert_eq!(stats.cross_file_references, 1);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].target, target_node);
    }

    #[test]
    fn test_pass4_unresolved_remains_unresolved() {
        let export_map = ExportMap::new(); // Empty
        let file_a = FileId::new(0);
        let source_node = NodeId::new(0, 1);

        let unresolved = vec![UnresolvedRef {
            source_node,
            target_name: "unknown::symbol".to_string(),
            kind: EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            source_file: file_a,
            line: 0,
            column: 0,
            has_location: false,
        }];

        let (stats, edges) = pass4_cross_file(&unresolved, &[], &export_map, None, file_a);

        assert_eq!(stats.unresolved_processed, 1);
        assert_eq!(stats.still_unresolved, 1);
        assert_eq!(stats.cross_file_calls, 0);
        assert!(edges.is_empty());
    }

    #[test]
    fn test_build_export_map() {
        let file_a = FileId::new(0);
        let file_b = FileId::new(1);

        let mut symbols_a = HashMap::new();
        symbols_a.insert("a::Foo".to_string(), NodeId::new(0, 1));
        symbols_a.insert("a::Bar".to_string(), NodeId::new(1, 1));

        let mut symbols_b = HashMap::new();
        symbols_b.insert("b::Baz".to_string(), NodeId::new(2, 1));

        let files = vec![(file_a, &symbols_a), (file_b, &symbols_b)];

        let export_map = build_export_map(files.into_iter());

        assert_eq!(export_map.len(), 3);
        assert!(export_map.contains("a::Foo"));
        assert!(export_map.contains("a::Bar"));
        assert!(export_map.contains("b::Baz"));
    }
}
