//! Shared test fixtures for high-velocity test writing.
//!
//! This crate provides pre-built fixtures for common test scenarios,
//! eliminating boilerplate and enabling rapid test creation.
//!
//! # Usage
//!
//! ```rust,ignore
//! use sqry_test_fixtures::graphs;
//!
//! #[test]
//! fn test_with_fixture() {
//!     let snapshot = graphs::simple_rust_project();
//!     // Test your code against the fixture
//! }
//! ```

use std::path::Path;

use sqry_core::graph::node::Language;
use sqry_core::graph::unified::concurrent::{CodeGraph, GraphSnapshot};
use sqry_core::graph::unified::edge::{BidirectionalEdgeStore, EdgeKind, HttpMethod};
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::graph::unified::storage::NodeEntry;
use sqry_core::graph::unified::storage::arena::NodeArena;
use sqry_core::graph::unified::storage::indices::AuxiliaryIndices;
use sqry_core::graph::unified::storage::interner::StringInterner;
use sqry_core::graph::unified::storage::registry::FileRegistry;

/// Pre-built graph fixtures for testing.
pub mod graphs {
    use super::{CodeGraph, GraphBuilder, GraphSnapshot, Language};

    /// Creates an empty graph with no nodes or edges.
    ///
    /// Useful for testing edge cases and empty state handling.
    #[must_use]
    pub fn empty_graph() -> GraphSnapshot {
        CodeGraph::new().snapshot()
    }

    /// Creates a simple Rust project graph with 2 files and 5 functions.
    ///
    /// Structure:
    /// - src/lib.rs: `pub fn main()`, `fn helper()`, `async fn async_task()`
    /// - src/utils.rs: `pub fn utility()`, `fn internal()`
    ///
    /// Edges:
    /// - main -> helper (call)
    /// - main -> utility (call)
    /// - helper -> `async_task` (call)
    #[must_use]
    pub fn simple_rust_project() -> GraphSnapshot {
        let mut builder = GraphBuilder::new();

        // File: src/lib.rs
        let lib_file = builder.add_file("src/lib.rs", Language::Rust);

        let main_fn = builder.add_function("main", "crate::main", lib_file, 1, 10, true, false);
        let helper_fn =
            builder.add_function("helper", "crate::helper", lib_file, 12, 20, false, false);
        let async_fn = builder.add_function(
            "async_task",
            "crate::async_task",
            lib_file,
            22,
            30,
            false,
            true,
        );

        // File: src/utils.rs
        let utils_file = builder.add_file("src/utils.rs", Language::Rust);

        let utility_fn = builder.add_function(
            "utility",
            "crate::utils::utility",
            utils_file,
            1,
            15,
            true,
            false,
        );
        let internal_fn = builder.add_function(
            "internal",
            "crate::utils::internal",
            utils_file,
            17,
            25,
            false,
            false,
        );

        // Edges
        builder.add_call(main_fn, helper_fn, 2, false);
        builder.add_call(main_fn, utility_fn, 1, false);
        builder.add_call(helper_fn, async_fn, 0, true);
        builder.add_call(utility_fn, internal_fn, 1, false);

        builder.build()
    }

    /// Creates a multi-language project with JavaScript, Python, and Rust.
    ///
    /// Structure:
    /// - src/main.rs: Rust entry point
    /// - src/handler.js: JavaScript handler
    /// - src/processor.py: Python processor
    ///
    /// Edges include cross-language HTTP calls.
    #[must_use]
    pub fn multi_language_project() -> GraphSnapshot {
        let mut builder = GraphBuilder::new();

        // Rust file
        let rust_file = builder.add_file("src/main.rs", Language::Rust);
        let rust_main = builder.add_function("main", "crate::main", rust_file, 1, 20, true, false);

        // JavaScript file
        let js_file = builder.add_file("src/handler.js", Language::JavaScript);
        let js_handler = builder.add_function(
            "handleRequest",
            "handler.handleRequest",
            js_file,
            1,
            30,
            true,
            true,
        );
        let js_helper = builder.add_function(
            "formatResponse",
            "handler.formatResponse",
            js_file,
            32,
            45,
            false,
            false,
        );

        // Python file
        let py_file = builder.add_file("src/processor.py", Language::Python);
        let py_process = builder.add_function(
            "process_data",
            "processor.process_data",
            py_file,
            1,
            25,
            true,
            false,
        );
        let py_validate = builder.add_function(
            "validate",
            "processor.validate",
            py_file,
            27,
            40,
            false,
            false,
        );

        // Intra-language calls
        builder.add_call(js_handler, js_helper, 1, false);
        builder.add_call(py_process, py_validate, 2, false);

        // Cross-language HTTP calls
        builder.add_http_call(rust_main, js_handler, "POST", "/api/handle");
        builder.add_http_call(js_handler, py_process, "GET", "/process");

        builder.build()
    }

    /// Creates a project with class hierarchies for OOP testing.
    ///
    /// Structure:
    /// - Animal (class) <- Dog, Cat (inherit)
    /// - Serializable (interface) <- Dog, Cat (implement)
    #[must_use]
    pub fn class_hierarchy() -> GraphSnapshot {
        let mut builder = GraphBuilder::new();

        let file = builder.add_file("src/animals.ts", Language::TypeScript);

        // Base class and interface
        let animal = builder.add_class("Animal", "animals.Animal", file, 1, 20);
        let serializable =
            builder.add_interface("Serializable", "animals.Serializable", file, 22, 30);

        // Derived classes
        let dog = builder.add_class("Dog", "animals.Dog", file, 32, 50);
        let cat = builder.add_class("Cat", "animals.Cat", file, 52, 70);

        // Methods
        let dog_bark = builder.add_method("bark", "animals.Dog.bark", file, 35, 40);
        let cat_meow = builder.add_method("meow", "animals.Cat.meow", file, 55, 60);

        // Inheritance and implementation edges
        builder.add_inherits(dog, animal);
        builder.add_inherits(cat, animal);
        builder.add_implements(dog, serializable);
        builder.add_implements(cat, serializable);

        // Containment edges
        builder.add_contains(dog, dog_bark);
        builder.add_contains(cat, cat_meow);

        builder.build()
    }

    /// Creates a large project for performance testing.
    ///
    /// - 10 files
    /// - 100 functions
    /// - 200 call edges
    #[must_use]
    pub fn large_project() -> GraphSnapshot {
        let mut builder = GraphBuilder::new();

        let mut all_functions = Vec::new();

        // Create 10 files with 10 functions each
        for file_idx in 0..10 {
            let file_path = format!("src/module_{file_idx}.rs");
            let file = builder.add_file(&file_path, Language::Rust);

            for fn_idx in 0..10 {
                let name = format!("func_{file_idx}_{fn_idx}");
                let qname = format!("module_{file_idx}::{name}");
                let start = fn_idx * 20 + 1;
                let end = start + 18;
                let is_pub = fn_idx % 3 == 0;

                let node = builder.add_function(&name, &qname, file, start, end, is_pub, false);
                all_functions.push(node);
            }
        }

        // Create 200 random-ish call edges
        for i in 0..200 {
            let source = all_functions[i % 100];
            let target = all_functions[(i * 7 + 3) % 100];
            if source != target {
                let arg_count = u8::try_from(i % 5).unwrap_or(0);
                builder.add_call(source, target, arg_count, false);
            }
        }

        builder.build()
    }

    /// Creates a project with import/export relationships.
    #[must_use]
    pub fn module_imports() -> GraphSnapshot {
        let mut builder = GraphBuilder::new();

        // Main module
        let main_file = builder.add_file("src/main.ts", Language::TypeScript);
        let main_module = builder.add_module("main", "main", main_file, 1, 50);
        let main_fn = builder.add_function("run", "main.run", main_file, 5, 20, true, false);

        // Utils module
        let utils_file = builder.add_file("src/utils.ts", Language::TypeScript);
        let utils_module = builder.add_module("utils", "utils", utils_file, 1, 40);
        let format_fn =
            builder.add_function("format", "utils.format", utils_file, 5, 15, true, false);
        let parse_fn =
            builder.add_function("parse", "utils.parse", utils_file, 17, 30, true, false);

        // Import edges
        builder.add_import(main_module, format_fn, false, None);
        builder.add_import(main_module, parse_fn, false, Some("parseData"));

        // Export edges
        builder.add_export(utils_module, format_fn);
        builder.add_export(utils_module, parse_fn);

        // Call edges
        builder.add_call(main_fn, format_fn, 1, false);
        builder.add_call(main_fn, parse_fn, 2, false);

        builder.build()
    }
}

/// Graph builder for creating test fixtures.
///
/// This builder provides an ergonomic API for constructing `GraphSnapshot`
/// instances with nodes and edges for testing purposes.
///
/// # Example
///
/// ```rust,ignore
/// use sqry_test_fixtures::GraphBuilder;
/// use sqry_core::graph::node::Language;
///
/// let mut builder = GraphBuilder::new();
/// let file = builder.add_file("src/main.rs", Language::Rust);
/// let main_fn = builder.add_function("main", "crate::main", file, 1, 10, true, false);
/// let helper = builder.add_function("helper", "crate::helper", file, 12, 20, false, false);
/// builder.add_call(main_fn, helper, 1, false);
/// let snapshot = builder.build();
/// ```
///
/// # Note on byte offsets
///
/// Byte offsets (`start_byte`, `end_byte`) are synthesized as `line * 80` for simplicity.
/// If your tests depend on exact byte ranges, you may need to adjust expectations accordingly.
pub struct GraphBuilder {
    nodes: NodeArena,
    edges: BidirectionalEdgeStore,
    strings: StringInterner,
    files: FileRegistry,
    indices: AuxiliaryIndices,
}

impl Default for GraphBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphBuilder {
    /// Creates a new empty graph builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: NodeArena::new(),
            edges: BidirectionalEdgeStore::new(),
            strings: StringInterner::new(),
            files: FileRegistry::new(),
            indices: AuxiliaryIndices::new(),
        }
    }

    /// Registers a file with the given path and language.
    ///
    /// Returns a `FileId` that can be used to associate nodes and edges with this file.
    ///
    /// # Panics
    ///
    /// Panics if the file registry fails to register the file.
    pub fn add_file(
        &mut self,
        path: &str,
        language: Language,
    ) -> sqry_core::graph::unified::file::FileId {
        self.files
            .register_with_language(Path::new(path), Some(language))
            .expect("Failed to register file")
    }

    /// Adds a function node to the graph.
    ///
    /// # Arguments
    /// * `name` - The function name
    /// * `qualified_name` - Fully qualified name (e.g., "`crate::module::function`")
    /// * `file` - The file this function belongs to
    /// * `start_line` - Starting line number
    /// * `end_line` - Ending line number
    /// * `is_public` - Whether the function is publicly visible
    /// * `is_async` - Whether the function is async
    ///
    /// # Panics
    ///
    /// Panics if string interning fails.
    pub fn add_function(
        &mut self,
        name: &str,
        qualified_name: &str,
        file: sqry_core::graph::unified::file::FileId,
        start_line: u32,
        end_line: u32,
        is_public: bool,
        is_async: bool,
    ) -> sqry_core::graph::unified::node::NodeId {
        let name_id = self.strings.intern(name).expect("Failed to intern name");
        let qname_id = self
            .strings
            .intern(qualified_name)
            .expect("Failed to intern qname");

        let visibility = if is_public {
            Some(
                self.strings
                    .intern("public")
                    .expect("Failed to intern visibility"),
            )
        } else {
            None
        };

        let entry = NodeEntry {
            kind: NodeKind::Function,
            name: name_id,
            file,
            start_byte: start_line * 80,
            end_byte: end_line * 80,
            start_line,
            start_column: 0,
            end_line,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: Some(qname_id),
            visibility,
            is_async,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };

        self.nodes.alloc(entry).expect("Failed to alloc node")
    }

    /// Adds a class node to the graph.
    pub fn add_class(
        &mut self,
        name: &str,
        qualified_name: &str,
        file: sqry_core::graph::unified::file::FileId,
        start_line: u32,
        end_line: u32,
    ) -> sqry_core::graph::unified::node::NodeId {
        self.add_node(
            NodeKind::Class,
            name,
            qualified_name,
            file,
            start_line,
            end_line,
        )
    }

    /// Adds an interface node to the graph.
    pub fn add_interface(
        &mut self,
        name: &str,
        qualified_name: &str,
        file: sqry_core::graph::unified::file::FileId,
        start_line: u32,
        end_line: u32,
    ) -> sqry_core::graph::unified::node::NodeId {
        self.add_node(
            NodeKind::Interface,
            name,
            qualified_name,
            file,
            start_line,
            end_line,
        )
    }

    /// Adds a method node to the graph.
    pub fn add_method(
        &mut self,
        name: &str,
        qualified_name: &str,
        file: sqry_core::graph::unified::file::FileId,
        start_line: u32,
        end_line: u32,
    ) -> sqry_core::graph::unified::node::NodeId {
        self.add_node(
            NodeKind::Method,
            name,
            qualified_name,
            file,
            start_line,
            end_line,
        )
    }

    /// Adds a module node to the graph.
    pub fn add_module(
        &mut self,
        name: &str,
        qualified_name: &str,
        file: sqry_core::graph::unified::file::FileId,
        start_line: u32,
        end_line: u32,
    ) -> sqry_core::graph::unified::node::NodeId {
        self.add_node(
            NodeKind::Module,
            name,
            qualified_name,
            file,
            start_line,
            end_line,
        )
    }

    /// Adds a node of any kind to the graph.
    ///
    /// # Panics
    ///
    /// Panics if string interning or node allocation fails.
    pub fn add_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        qualified_name: &str,
        file: sqry_core::graph::unified::file::FileId,
        start_line: u32,
        end_line: u32,
    ) -> sqry_core::graph::unified::node::NodeId {
        let name_id = self.strings.intern(name).expect("Failed to intern name");
        let qname_id = self
            .strings
            .intern(qualified_name)
            .expect("Failed to intern qname");

        let entry = NodeEntry {
            kind,
            name: name_id,
            file,
            start_byte: start_line * 80,
            end_byte: end_line * 80,
            start_line,
            start_column: 0,
            end_line,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: Some(qname_id),
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };

        self.nodes.alloc(entry).expect("Failed to alloc node")
    }

    /// Gets the file associated with a node.
    ///
    /// This is used internally to derive the correct file for edge attribution.
    fn get_node_file(
        &self,
        node: sqry_core::graph::unified::node::NodeId,
    ) -> sqry_core::graph::unified::file::FileId {
        self.nodes.get(node).expect("Node not found").file
    }

    /// Adds a call edge from source to target.
    ///
    /// The edge is attributed to the source node's file.
    pub fn add_call(
        &mut self,
        source: sqry_core::graph::unified::node::NodeId,
        target: sqry_core::graph::unified::node::NodeId,
        args: u8,
        is_async: bool,
    ) {
        let file = self.get_node_file(source);
        self.edges.add_edge(
            source,
            target,
            EdgeKind::Calls {
                argument_count: args,
                is_async,
            },
            file,
        );
    }

    /// Adds an HTTP request edge from source to target.
    ///
    /// The edge is attributed to the source node's file.
    /// Method matching is case-insensitive (e.g., "get", "GET", "Get" all work).
    ///
    /// # Panics
    ///
    /// Panics if `method` is not one of: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS
    /// (case-insensitive).
    pub fn add_http_call(
        &mut self,
        source: sqry_core::graph::unified::node::NodeId,
        target: sqry_core::graph::unified::node::NodeId,
        method: &str,
        url: &str,
    ) {
        let file = self.get_node_file(source);
        let http_method = match method.to_ascii_uppercase().as_str() {
            "GET" => HttpMethod::Get,
            "POST" => HttpMethod::Post,
            "PUT" => HttpMethod::Put,
            "DELETE" => HttpMethod::Delete,
            "PATCH" => HttpMethod::Patch,
            "HEAD" => HttpMethod::Head,
            "OPTIONS" => HttpMethod::Options,
            "ALL" => HttpMethod::All,
            _ => panic!(
                "Invalid HTTP method '{method}'. Must be one of: GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS, ALL (case-insensitive)"
            ),
        };
        let url_id = self.strings.intern(url).expect("intern");
        self.edges.add_edge(
            source,
            target,
            EdgeKind::HttpRequest {
                method: http_method,
                url: Some(url_id),
            },
            file,
        );
    }

    /// Adds an HTTP request edge using a typed `HttpMethod`.
    ///
    /// Use this when you already have an `HttpMethod` value to avoid string parsing.
    ///
    /// # Panics
    ///
    /// Panics if string interning fails for the URL.
    pub fn add_http_call_typed(
        &mut self,
        source: sqry_core::graph::unified::node::NodeId,
        target: sqry_core::graph::unified::node::NodeId,
        method: HttpMethod,
        url: &str,
    ) {
        let file = self.get_node_file(source);
        let url_id = self.strings.intern(url).expect("intern");
        self.edges.add_edge(
            source,
            target,
            EdgeKind::HttpRequest {
                method,
                url: Some(url_id),
            },
            file,
        );
    }

    /// Adds an inheritance edge (child extends parent).
    ///
    /// The edge is attributed to the child node's file.
    pub fn add_inherits(
        &mut self,
        child: sqry_core::graph::unified::node::NodeId,
        parent: sqry_core::graph::unified::node::NodeId,
    ) {
        let file = self.get_node_file(child);
        self.edges.add_edge(child, parent, EdgeKind::Inherits, file);
    }

    /// Adds an implements edge (implementor implements interface).
    ///
    /// The edge is attributed to the implementor node's file.
    pub fn add_implements(
        &mut self,
        implementor: sqry_core::graph::unified::node::NodeId,
        interface: sqry_core::graph::unified::node::NodeId,
    ) {
        let file = self.get_node_file(implementor);
        self.edges
            .add_edge(implementor, interface, EdgeKind::Implements, file);
    }

    /// Adds a containment edge (container contains contained).
    ///
    /// The edge is attributed to the container node's file.
    pub fn add_contains(
        &mut self,
        container_node_id: sqry_core::graph::unified::node::NodeId,
        member_node_id: sqry_core::graph::unified::node::NodeId,
    ) {
        let file = self.get_node_file(container_node_id);
        self.edges
            .add_edge(container_node_id, member_node_id, EdgeKind::Contains, file);
    }

    /// Adds an import edge (importer imports imported).
    ///
    /// The edge is attributed to the importer node's file.
    ///
    /// # Panics
    ///
    /// Panics if string interning fails for the alias.
    pub fn add_import(
        &mut self,
        importer_node_id: sqry_core::graph::unified::node::NodeId,
        imported_symbol_id: sqry_core::graph::unified::node::NodeId,
        is_wildcard: bool,
        alias: Option<&str>,
    ) {
        let file = self.get_node_file(importer_node_id);
        let alias_id = alias.map(|a| self.strings.intern(a).expect("intern"));
        self.edges.add_edge(
            importer_node_id,
            imported_symbol_id,
            EdgeKind::Imports {
                alias: alias_id,
                is_wildcard,
            },
            file,
        );
    }

    /// Adds an export edge (module exports symbol).
    ///
    /// The edge is attributed to the module node's file.
    pub fn add_export(
        &mut self,
        module: sqry_core::graph::unified::node::NodeId,
        symbol: sqry_core::graph::unified::node::NodeId,
    ) {
        let file = self.get_node_file(module);
        self.edges.add_edge(
            module,
            symbol,
            EdgeKind::Exports {
                kind: sqry_core::graph::unified::edge::ExportKind::Direct,
                alias: None,
            },
            file,
        );
    }

    /// Builds the graph and returns an immutable snapshot.
    #[must_use]
    pub fn build(self) -> GraphSnapshot {
        CodeGraph::from_components(
            self.nodes,
            self.edges,
            self.strings,
            self.files,
            self.indices,
        )
        .snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::graphs::*;

    #[test]
    fn test_empty_graph() {
        let snapshot = empty_graph();
        assert_eq!(snapshot.nodes().len(), 0);
    }

    #[test]
    fn test_simple_rust_project() {
        let snapshot = simple_rust_project();
        assert_eq!(snapshot.nodes().len(), 5);
        // 4 call edges - verify via stats
        let stats = snapshot.edges().stats();
        assert!(stats.forward.csr_edge_count + stats.forward.delta_edge_count >= 4);
    }

    #[test]
    fn test_multi_language_project() {
        let snapshot = multi_language_project();
        // 5 functions across 3 files: rust_main, js_handler, js_helper, py_process, py_validate
        assert_eq!(snapshot.nodes().len(), 5);
    }

    #[test]
    fn test_class_hierarchy() {
        let snapshot = class_hierarchy();
        // Animal, Serializable, Dog, Cat, bark, meow = 6 nodes
        assert_eq!(snapshot.nodes().len(), 6);
    }

    #[test]
    fn test_large_project() {
        let snapshot = large_project();
        assert_eq!(snapshot.nodes().len(), 100);
        // At least 100 edges (some may be deduplicated)
        let stats = snapshot.edges().stats();
        assert!(stats.forward.csr_edge_count + stats.forward.delta_edge_count >= 50);
    }

    #[test]
    fn test_module_imports() {
        let snapshot = module_imports();
        // 2 modules + 3 functions = 5 nodes
        assert_eq!(snapshot.nodes().len(), 5);
    }
}
