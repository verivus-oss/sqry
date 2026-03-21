//! Tree-sitter query helpers and language-specific query sets.
//!
//! Queries live alongside the shared engine so adapters can reference them
//! without duplicating S-expression strings in each language crate. The helper
//! utilities here compile queries on demand and return the captured nodes so the
//! engines can assemble relation edges.

use std::collections::HashSet;

use tree_sitter::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, Tree};

/// Describes a single tree-sitter query and the capture that callers care
/// about.
#[derive(Debug, Clone, Copy)]
pub struct QuerySpec {
    /// Human-readable name used in error messages.
    pub name: &'static str,
    /// Capture identifier that should be collected from matches.
    pub capture: &'static str,
    /// S-expression query source.
    pub source: &'static str,
}

impl QuerySpec {
    /// Execute the query against `tree`, collecting all nodes matching the
    /// configured capture. Duplicate nodes (based on start byte) are filtered
    /// so downstream consumers don't have to deduplicate work.
    ///
    /// # Errors
    ///
    /// Returns an error when the query source cannot be compiled for the
    /// current tree-sitter language or when the requested capture name is not
    /// present in the compiled query.
    pub fn run<'tree>(
        &self,
        tree: &'tree Tree,
        content: &[u8],
    ) -> Result<Vec<Node<'tree>>, String> {
        let language = tree.language();
        let query = Query::new(&language, self.source)
            .map_err(|err| format!("failed to compile query `{}`: {err}", self.name))?;
        let capture_index = query
            .capture_names()
            .iter()
            .position(|name| *name == self.capture)
            .ok_or_else(|| format!("query `{}` missing capture `{}`", self.name, self.capture))?;

        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), content);
        let mut seen_offsets = HashSet::new();
        let mut nodes = Vec::new();

        while let Some(m) = matches.next() {
            for capture in m.captures {
                if capture.index as usize == capture_index
                    && seen_offsets.insert(capture.node.start_byte())
                {
                    nodes.push(capture.node);
                }
            }
        }

        Ok(nodes)
    }
}

/// Query set used by the import extractor. Additional relation-specific query
/// collections will join this as the engine gains native query support.
#[derive(Debug)]
pub struct ImportQuerySet {
    /// Query specs that match import statements in the source file.
    pub statements: &'static [QuerySpec],
}

/// Query set used by the export extractor.
#[derive(Debug)]
pub struct ExportQuerySet {
    /// Query specs that match export statements in the source file.
    pub statements: &'static [QuerySpec],
}

/// Query set used by the call extractor.
#[derive(Debug)]
pub struct CallQuerySet {
    /// Query specs that match top-level callable constructs.
    pub roots: &'static [QuerySpec],
}

pub mod typescript {
    //! TypeScript-specific queries consumed by the shared relations engine.

    use super::{CallQuerySet, ExportQuerySet, ImportQuerySet, QuerySpec};

    /// Query set that extracts every `import_statement` in a TypeScript source
    /// file.
    pub const IMPORT_QUERIES: ImportQuerySet = ImportQuerySet {
        statements: &[QuerySpec {
            name: "typescript_import_statements",
            capture: "import.statement",
            source: include_str!("typescript/imports.scm"),
        }],
    };

    /// Query set that extracts every `export_statement`.
    pub const EXPORT_QUERIES: ExportQuerySet = ExportQuerySet {
        statements: &[QuerySpec {
            name: "typescript_export_statements",
            capture: "export.statement",
            source: include_str!("typescript/exports.scm"),
        }],
    };

    /// Query set that captures top-level call roots (functions/classes).
    pub const CALL_QUERIES: CallQuerySet = CallQuerySet {
        roots: &[QuerySpec {
            name: "typescript_call_roots",
            capture: "call.root",
            source: include_str!("typescript/calls.scm"),
        }],
    };
}

pub mod javascript {
    //! JavaScript-specific queries consumed by the shared relations engine.

    use super::{CallQuerySet, ExportQuerySet, ImportQuerySet, QuerySpec};

    /// Query set for extracting JavaScript import statements.
    pub const IMPORT_QUERIES: ImportQuerySet = ImportQuerySet {
        statements: &[QuerySpec {
            name: "javascript_import_statements",
            capture: "import.statement",
            source: include_str!("javascript/imports.scm"),
        }],
    };

    /// Query set for extracting JavaScript export statements.
    pub const EXPORT_QUERIES: ExportQuerySet = ExportQuerySet {
        statements: &[QuerySpec {
            name: "javascript_export_statements",
            capture: "export.statement",
            source: include_str!("javascript/exports.scm"),
        }],
    };

    /// Query set for extracting JavaScript call roots.
    pub const CALL_QUERIES: CallQuerySet = CallQuerySet {
        roots: &[QuerySpec {
            name: "javascript_call_roots",
            capture: "call.root",
            source: include_str!("javascript/calls.scm"),
        }],
    };
}

pub mod csharp {
    //! C# query sets used by the shared relations adapters.

    use super::{CallQuerySet, ImportQuerySet, QuerySpec};

    /// Query set for extracting C# using directives.
    pub const IMPORT_QUERIES: ImportQuerySet = ImportQuerySet {
        statements: &[QuerySpec {
            name: "csharp_using_directives",
            capture: "import.directive",
            source: include_str!("csharp/imports.scm"),
        }],
    };

    /// Query set for extracting C# call roots.
    pub const CALL_QUERIES: CallQuerySet = CallQuerySet {
        roots: &[QuerySpec {
            name: "csharp_call_roots",
            capture: "call.root",
            source: include_str!("csharp/calls.scm"),
        }],
    };
}

pub mod go {
    //! Go query sets used by the shared relations adapters.

    use super::{CallQuerySet, ExportQuerySet, ImportQuerySet, QuerySpec};

    /// Query set for extracting Go import declarations.
    pub const IMPORT_QUERIES: ImportQuerySet = ImportQuerySet {
        statements: &[QuerySpec {
            name: "go_import_declarations",
            capture: "import.declaration",
            source: include_str!("go/imports.scm"),
        }],
    };

    /// Query set for extracting Go call roots.
    pub const CALL_QUERIES: CallQuerySet = CallQuerySet {
        roots: &[QuerySpec {
            name: "go_call_roots",
            capture: "call.root",
            source: include_str!("go/calls.scm"),
        }],
    };

    /// Query set for extracting Go exported declarations.
    pub const EXPORT_QUERIES: ExportQuerySet = ExportQuerySet {
        statements: &[QuerySpec {
            name: "go_export_declarations",
            capture: "export.declaration",
            source: include_str!("go/exports.scm"),
        }],
    };
}
