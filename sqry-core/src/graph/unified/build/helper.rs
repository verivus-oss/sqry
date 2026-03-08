//! Helper utilities for `GraphBuilder` implementations.
//!
//! This module provides high-level abstractions that make it easier to
//! implement `GraphBuilder::build_graph()` using the `StagingGraph` API.
//!
//! # Overview
//!
//! The [`GraphBuildHelper`] wraps a `&mut StagingGraph` and provides:
//! - Local string interning with `StringId` tracking
//! - Qualified name to `NodeId` mapping
//! - High-level node creation methods
//! - High-level edge creation methods
#![allow(clippy::similar_names)] // Domain terminology uses caller/callee and importer/imported pairs.
//!
//! # Usage
//!
//! ```ignore
//! fn build_graph(
//!     &self,
//!     tree: &Tree,
//!     content: &[u8],
//!     file: &Path,
//!     staging: &mut StagingGraph,
//! ) -> GraphResult<()> {
//!     let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
//!
//!     // Create function nodes
//!     let main_id = helper.add_function("main", None, false, false)?;
//!     let helper_id = helper.add_function("helper", None, false, false)?;
//!
//!     // Create call edge
//!     helper.add_call_edge(main_id, helper_id);
//!
//!     Ok(())
//! }
//! ```
//!
//! This helper provides a high-level API that mirrors the patterns plugins use
//! with `StagingGraph`, reducing boilerplate in `GraphBuilder` implementations.

use std::collections::HashMap;
use std::path::Path;

use super::super::edge::kind::{LifetimeConstraintKind, MacroExpansionKind, TypeOfContext};
use super::staging::StagingGraph;
use crate::graph::node::{Language, Span};
use crate::graph::unified::edge::{EdgeKind, ExportKind, FfiConvention, HttpMethod, TableWriteOp};
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::storage::NodeEntry;
use crate::graph::unified::string::StringId;

/// Helper for building graphs in `GraphBuilder` implementations.
///
/// Provides high-level abstractions over `StagingGraph` that handle:
/// - String interning with local ID tracking
/// - Qualified name deduplication
/// - Node and edge creation with proper types
#[derive(Debug)]
pub struct GraphBuildHelper<'a> {
    /// The underlying staging graph.
    staging: &'a mut StagingGraph,
    /// Language for this file.
    language: Language,
    /// File ID (pre-allocated).
    file_id: FileId,
    /// File path for error messages.
    file_path: String,
    /// Local string interning: string value -> local `StringId`.
    string_cache: HashMap<String, StringId>,
    /// Next local string ID to allocate.
    next_string_id: u32,
    /// Qualified name -> `NodeId` mapping for deduplication.
    node_cache: HashMap<(String, NodeKind), NodeId>,
}

impl<'a> GraphBuildHelper<'a> {
    /// Create a new helper for the given staging graph and file.
    ///
    /// The `file_id` should be pre-allocated by the caller (typically 0 for
    /// per-file staging buffers).
    pub fn new(staging: &'a mut StagingGraph, file: &Path, language: Language) -> Self {
        Self {
            staging,
            language,
            file_id: FileId::new(0), // Per-file staging uses local file ID 0
            file_path: file.display().to_string(),
            string_cache: HashMap::new(),
            next_string_id: 0,
            node_cache: HashMap::new(),
        }
    }

    /// Create a helper with a specific file ID.
    pub fn with_file_id(
        staging: &'a mut StagingGraph,
        file: &Path,
        language: Language,
        file_id: FileId,
    ) -> Self {
        Self {
            staging,
            language,
            file_id,
            file_path: file.display().to_string(),
            string_cache: HashMap::new(),
            next_string_id: 0,
            node_cache: HashMap::new(),
        }
    }

    /// Get the language for this helper.
    #[must_use]
    pub fn language(&self) -> Language {
        self.language
    }

    /// Get the file ID for this helper.
    #[must_use]
    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    /// Get the file path.
    #[must_use]
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// Intern a string and get a local `StringId`.
    ///
    /// Strings are deduplicated: calling with the same value returns the same ID.
    /// The local `StringId` is passed to the staging graph so that during
    /// `commit_strings()`, a remap table from local to global IDs can be built.
    pub fn intern(&mut self, s: &str) -> StringId {
        if let Some(&id) = self.string_cache.get(s) {
            return id;
        }

        let id = StringId::new_local(self.next_string_id);
        self.next_string_id += 1;
        self.string_cache.insert(s.to_string(), id);
        // Pass the local_id to staging so it can build the remap table during commit
        self.staging.intern_string(id, s.to_string());
        id
    }

    /// Check if a node with the given qualified name already exists.
    #[must_use]
    pub fn has_node(&self, qualified_name: &str) -> bool {
        self.node_cache
            .keys()
            .any(|(name, _)| name == qualified_name)
    }

    /// Get an existing node by qualified name.
    #[must_use]
    pub fn get_node(&self, qualified_name: &str) -> Option<NodeId> {
        self.node_cache
            .iter()
            .find_map(|((name, _), id)| (name == qualified_name).then_some(*id))
    }

    /// Check if a node with the given qualified name and kind already exists.
    #[must_use]
    pub fn has_node_with_kind(&self, qualified_name: &str, kind: NodeKind) -> bool {
        self.node_cache
            .contains_key(&(qualified_name.to_string(), kind))
    }

    /// Get an existing node by qualified name and kind.
    #[must_use]
    pub fn get_node_with_kind(&self, qualified_name: &str, kind: NodeKind) -> Option<NodeId> {
        self.node_cache
            .get(&(qualified_name.to_string(), kind))
            .copied()
    }

    /// Add a function node with the given qualified name.
    ///
    /// Returns the `NodeId` (creating the node if it doesn't exist).
    pub fn add_function(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_unsafe: bool,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Function,
            &[("async", is_async), ("unsafe", is_unsafe)],
            None,
            None,
        )
    }

    /// Add a function node with visibility.
    ///
    /// Returns the `NodeId` (creating the node if it doesn't exist).
    pub fn add_function_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_unsafe: bool,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Function,
            &[("async", is_async), ("unsafe", is_unsafe)],
            visibility,
            None,
        )
    }

    /// Add a function node with signature (return type).
    ///
    /// The signature is used for `returns:` queries.
    /// Returns the `NodeId` (creating the node if it doesn't exist).
    pub fn add_function_with_signature(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_unsafe: bool,
        visibility: Option<&str>,
        signature: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Function,
            &[("async", is_async), ("unsafe", is_unsafe)],
            visibility,
            signature,
        )
    }

    /// Add a method node with the given qualified name.
    pub fn add_method(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_static: bool,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Method,
            &[("async", is_async), ("static", is_static)],
            None,
            None,
        )
    }

    /// Add a method node with visibility.
    pub fn add_method_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_static: bool,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Method,
            &[("async", is_async), ("static", is_static)],
            visibility,
            None,
        )
    }

    /// Add a method node with signature (return type).
    ///
    /// The signature is used for `returns:` queries.
    /// Returns the `NodeId` (creating the node if it doesn't exist).
    pub fn add_method_with_signature(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_static: bool,
        visibility: Option<&str>,
        signature: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Method,
            &[("async", is_async), ("static", is_static)],
            visibility,
            signature,
        )
    }

    /// Add a class node.
    pub fn add_class(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Class, &[], None, None)
    }

    /// Add a class node with visibility.
    pub fn add_class_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Class, &[], visibility, None)
    }

    /// Add a struct node.
    pub fn add_struct(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Struct, &[], None, None)
    }

    /// Add a struct node with visibility.
    pub fn add_struct_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Struct,
            &[],
            visibility,
            None,
        )
    }

    /// Add a module node.
    pub fn add_module(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Module, &[], None, None)
    }

    /// Add a resource node.
    pub fn add_resource(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Resource, &[], None, None)
    }

    /// Add an endpoint node for HTTP route handlers.
    ///
    /// The qualified name should follow the convention `route::{METHOD}::{path}`,
    /// for example `route::GET::/api/users` or `route::POST::/api/items`.
    ///
    /// Endpoint nodes are used by Pass 5 (cross-language linking) to match
    /// HTTP requests from client code to server-side route handlers.
    pub fn add_endpoint(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Endpoint, &[], None, None)
    }

    /// Add an import node.
    pub fn add_import(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Import, &[], None, None)
    }

    /// Add a variable node.
    pub fn add_variable(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Variable, &[], None, None)
    }

    /// Add a constant node.
    pub fn add_constant(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Constant, &[], None, None)
    }

    /// Add a constant node with visibility.
    pub fn add_constant_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Constant,
            &[],
            visibility,
            None,
        )
    }

    /// Add a constant node with static and visibility attributes.
    pub fn add_constant_with_static_and_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_static: bool,
        visibility: Option<&str>,
    ) -> NodeId {
        let attrs: &[(&str, bool)] = if is_static { &[("static", true)] } else { &[] };
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Constant,
            attrs,
            visibility,
            None,
        )
    }

    /// Add a property node with static and visibility attributes.
    pub fn add_property_with_static_and_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_static: bool,
        visibility: Option<&str>,
    ) -> NodeId {
        let attrs: &[(&str, bool)] = if is_static { &[("static", true)] } else { &[] };
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Property,
            attrs,
            visibility,
            None,
        )
    }

    /// Add an enum node.
    pub fn add_enum(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Enum, &[], None, None)
    }

    /// Add an enum node with visibility.
    pub fn add_enum_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Enum, &[], visibility, None)
    }

    /// Add an interface/trait node.
    pub fn add_interface(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Interface, &[], None, None)
    }

    /// Add an interface/trait node with visibility.
    pub fn add_interface_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Interface,
            &[],
            visibility,
            None,
        )
    }

    /// Add a type alias node.
    pub fn add_type(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Type, &[], None, None)
    }

    /// Add a type alias node with visibility.
    pub fn add_type_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Type, &[], visibility, None)
    }

    /// Add a lifetime node.
    pub fn add_lifetime(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Lifetime, &[], None, None)
    }

    /// Add a lifetime constraint edge.
    pub fn add_lifetime_constraint_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        constraint_kind: LifetimeConstraintKind,
    ) {
        self.staging.add_edge(
            source,
            target,
            EdgeKind::LifetimeConstraint { constraint_kind },
            self.file_id,
        );
    }

    /// Add a trait method binding edge.
    ///
    /// This edge represents the resolution of a trait method call to a concrete
    /// implementation.
    pub fn add_trait_method_binding_edge(
        &mut self,
        caller: NodeId,
        callee: NodeId,
        trait_name: &str,
        impl_type: &str,
        is_ambiguous: bool,
    ) {
        let trait_name_id = self.intern(trait_name);
        let impl_type_id = self.intern(impl_type);
        self.staging.add_edge(
            caller,
            callee,
            EdgeKind::TraitMethodBinding {
                trait_name: trait_name_id,
                impl_type: impl_type_id,
                is_ambiguous,
            },
            self.file_id,
        );
    }

    /// Add a macro expansion edge.
    ///
    /// Represents the expansion of a macro invocation to its generated code.
    /// Only available when macro expansion is enabled.
    ///
    /// # Arguments
    ///
    /// * `invocation` - The macro invocation site node (e.g., derive attribute or macro call)
    /// * `expansion` - The macro definition or generated code node
    /// * `expansion_kind` - The kind of macro expansion (Derive, Attribute, Declarative, Function)
    /// * `is_verified` - Whether the expansion has been verified (requires `cargo expand`)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // #[derive(Debug)] on a struct
    /// let struct_id = helper.add_struct("MyStruct", Some(span));
    /// let derive_macro_id = helper.add_node("MyStruct::derive_Debug", None, NodeKind::Macro);
    /// helper.add_macro_expansion_edge(
    ///     struct_id,
    ///     derive_macro_id,
    ///     MacroExpansionKind::Derive,
    ///     false,
    /// );
    /// ```
    pub fn add_macro_expansion_edge(
        &mut self,
        invocation: NodeId,
        expansion: NodeId,
        expansion_kind: MacroExpansionKind,
        is_verified: bool,
    ) {
        self.staging.add_edge(
            invocation,
            expansion,
            EdgeKind::MacroExpansion {
                expansion_kind,
                is_verified,
            },
            self.file_id,
        );
    }

    /// Add a generic node with custom kind.
    pub fn add_node(&mut self, qualified_name: &str, span: Option<Span>, kind: NodeKind) -> NodeId {
        self.add_node_internal(qualified_name, span, kind, &[], None, None)
    }

    /// Add a generic node with visibility.
    pub fn add_node_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(qualified_name, span, kind, &[], visibility, None)
    }

    /// Internal helper for adding nodes.
    ///
    /// Applies attributes to the node entry:
    /// - `"async"` → `NodeEntry::with_async(true/false)`
    /// - `"static"` → `NodeEntry::with_static(true/false)`
    /// - `"unsafe"` → `NodeEntry::with_unsafe(true/false)`
    ///
    /// When `signature` is `Some`, the signature field is set on the node for
    /// `returns:` queries.
    fn add_node_internal(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        attributes: &[(&str, bool)],
        visibility: Option<&str>,
        signature: Option<&str>,
    ) -> NodeId {
        let mut is_async = false;
        let mut is_static = false;
        let mut is_unsafe = false;
        for &(key, value) in attributes {
            match key {
                "async" => is_async |= value,
                "static" => is_static |= value,
                "unsafe" => is_unsafe |= value,
                _ => {}
            }
        }

        // Check cache first
        if let Some(&id) = self.node_cache.get(&(qualified_name.to_string(), kind)) {
            let visibility_id = visibility.map(|vis| self.intern(vis));
            let signature_id = signature.map(|sig| self.intern(sig));
            self.staging.update_node_entry(
                id,
                span,
                is_async,
                is_static,
                is_unsafe,
                visibility_id,
                signature_id,
            );
            return id;
        }

        // Intern the qualified name
        let name_id = self.intern(qualified_name);

        // Create node entry
        let mut entry = NodeEntry::new(kind, name_id, self.file_id);

        // Set span if provided
        if let Some(s) = span {
            let start_line = u32::try_from(s.start.line.saturating_add(1)).unwrap_or(u32::MAX);
            let start_column = u32::try_from(s.start.column).unwrap_or(u32::MAX);
            let end_line = u32::try_from(s.end.line.saturating_add(1)).unwrap_or(u32::MAX);
            let end_column = u32::try_from(s.end.column).unwrap_or(u32::MAX);
            entry = entry.with_location(start_line, start_column, end_line, end_column);
        }

        // Apply attributes to node entry
        if is_async {
            entry = entry.with_async(true);
        }
        if is_static {
            entry = entry.with_static(true);
        }
        if is_unsafe {
            entry = entry.with_unsafe(true);
        }

        // Apply visibility if provided
        if let Some(vis) = visibility {
            let vis_id = self.intern(vis);
            entry = entry.with_visibility(vis_id);
        }

        // Apply signature (return type) if provided
        if let Some(sig) = signature {
            let sig_id = self.intern(sig);
            entry = entry.with_signature(sig_id);
        }

        // Stage the node
        let node_id = self.staging.add_node(entry);

        // Cache for deduplication
        self.node_cache
            .insert((qualified_name.to_string(), kind), node_id);

        node_id
    }

    /// Add a call edge from caller to callee.
    pub fn add_call_edge(&mut self, caller: NodeId, callee: NodeId) {
        self.add_call_edge_with_span(caller, callee, Vec::new());
    }

    /// Add a call edge from caller to callee with source span information.
    ///
    /// The span should point to the call site location in source code.
    ///
    /// # Note
    ///
    /// This method uses default metadata (`argument_count: 255` sentinel for unknown, `is_async: false`).
    /// Use [`add_call_edge_full`](Self::add_call_edge_full) when you need to specify
    /// argument count or async status explicitly.
    pub fn add_call_edge_with_span(
        &mut self,
        caller: NodeId,
        callee: NodeId,
        spans: Vec<crate::graph::node::Span>,
    ) {
        self.staging.add_edge_with_spans(
            caller,
            callee,
            EdgeKind::Calls {
                argument_count: 255,
                is_async: false,
            },
            self.file_id,
            spans,
        );
    }

    /// Add a call edge with full metadata.
    ///
    /// Use this method when you know the argument count or when the call is async.
    /// For calls where metadata is unknown, use [`add_call_edge`](Self::add_call_edge)
    /// which uses default values (`argument_count: 255` sentinel, `is_async: false`).
    ///
    /// # Arguments
    ///
    /// * `caller` - The node making the call
    /// * `callee` - The node being called
    /// * `argument_count` - Number of arguments in the call (0-254, use 255 for unknown)
    /// * `is_async` - Whether this is an async/await call
    ///
    /// # Canonical Usage
    ///
    /// | Scenario | Method |
    /// |----------|--------|
    /// | Argument count known, sync call | `add_call_edge_full(caller, callee, arg_count, false)` |
    /// | Argument count known, async call | `add_call_edge_full(caller, callee, arg_count, true)` |
    /// | Argument count unknown, sync call | `add_call_edge(caller, callee)` or `add_call_edge_full(caller, callee, 255, false)` |
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Function call with 3 arguments
    /// helper.add_call_edge_full(main_id, helper_id, 3, false);
    ///
    /// // Async call with 1 argument
    /// helper.add_call_edge_full(main_id, async_fn_id, 1, true);
    /// ```
    pub fn add_call_edge_full(
        &mut self,
        caller: NodeId,
        callee: NodeId,
        argument_count: u8,
        is_async: bool,
    ) {
        self.staging.add_edge(
            caller,
            callee,
            EdgeKind::Calls {
                argument_count,
                is_async,
            },
            self.file_id,
        );
    }

    /// Add a call edge with full metadata and source span information.
    ///
    /// Combines the functionality of [`add_call_edge_full`](Self::add_call_edge_full)
    /// and span tracking.
    pub fn add_call_edge_full_with_span(
        &mut self,
        caller: NodeId,
        callee: NodeId,
        argument_count: u8,
        is_async: bool,
        spans: Vec<crate::graph::node::Span>,
    ) {
        self.staging.add_edge_with_spans(
            caller,
            callee,
            EdgeKind::Calls {
                argument_count,
                is_async,
            },
            self.file_id,
            spans,
        );
    }

    /// Add a database table read edge (SQL).
    pub fn add_table_read_edge_with_span(
        &mut self,
        reader: NodeId,
        table: NodeId,
        table_name: &str,
        schema: Option<&str>,
        spans: Vec<crate::graph::node::Span>,
    ) {
        let table_name_id = self.intern(table_name);
        let schema_id = schema.map(|s| self.intern(s));
        self.staging.add_edge_with_spans(
            reader,
            table,
            EdgeKind::TableRead {
                table_name: table_name_id,
                schema: schema_id,
            },
            self.file_id,
            spans,
        );
    }

    /// Add a database table write edge (SQL).
    pub fn add_table_write_edge_with_span(
        &mut self,
        writer: NodeId,
        table: NodeId,
        table_name: &str,
        schema: Option<&str>,
        operation: TableWriteOp,
        spans: Vec<crate::graph::node::Span>,
    ) {
        let table_name_id = self.intern(table_name);
        let schema_id = schema.map(|s| self.intern(s));
        self.staging.add_edge_with_spans(
            writer,
            table,
            EdgeKind::TableWrite {
                table_name: table_name_id,
                schema: schema_id,
                operation,
            },
            self.file_id,
            spans,
        );
    }

    /// Add a database trigger relationship edge (SQL).
    ///
    /// Convention: `trigger -> table` with `EdgeKind::TriggeredBy`.
    pub fn add_triggered_by_edge_with_span(
        &mut self,
        trigger: NodeId,
        table: NodeId,
        trigger_name: &str,
        schema: Option<&str>,
        spans: Vec<crate::graph::node::Span>,
    ) {
        let trigger_name_id = self.intern(trigger_name);
        let schema_id = schema.map(|s| self.intern(s));
        self.staging.add_edge_with_spans(
            trigger,
            table,
            EdgeKind::TriggeredBy {
                trigger_name: trigger_name_id,
                schema: schema_id,
            },
            self.file_id,
            spans,
        );
    }

    /// Add an import edge from importer to imported module/symbol.
    ///
    /// This method uses default metadata (`alias: None`, `is_wildcard: false`).
    /// Use [`add_import_edge_full`](Self::add_import_edge_full) when importing
    /// with an alias or for wildcard imports.
    pub fn add_import_edge(&mut self, importer: NodeId, imported: NodeId) {
        self.staging.add_edge(
            importer,
            imported,
            EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            self.file_id,
        );
    }

    /// Add an import edge with full metadata.
    ///
    /// Use this method when the import has an alias or is a wildcard import.
    /// For simple imports without alias or wildcard, use [`add_import_edge`](Self::add_import_edge).
    ///
    /// # Arguments
    ///
    /// * `importer` - The node importing (e.g., module or file)
    /// * `imported` - The node being imported
    /// * `alias` - Optional alias string (e.g., for `import { foo as bar }`, alias is "bar")
    /// * `is_wildcard` - Whether this is a wildcard import (e.g., `import *`)
    ///
    /// # Canonical Usage
    ///
    /// | Import Syntax | Method |
    /// |---------------|--------|
    /// | `import foo` | `add_import_edge(importer, imported)` |
    /// | `import foo as bar` | `add_import_edge_full(importer, imported, Some("bar"), false)` |
    /// | `import *` / `import *.*` | `add_import_edge_full(importer, imported, None, true)` |
    /// | `import * as ns` | `add_import_edge_full(importer, imported, Some("ns"), true)` |
    ///
    /// # Example
    ///
    /// ```ignore
    /// // import { HashMap as Map } from "std::collections"
    /// let alias_id = helper.intern("Map");
    /// helper.add_import_edge_full(module_id, hashmap_id, Some("Map"), false);
    ///
    /// // import * from "lodash"
    /// helper.add_import_edge_full(module_id, lodash_id, None, true);
    /// ```
    pub fn add_import_edge_full(
        &mut self,
        importer: NodeId,
        imported: NodeId,
        alias: Option<&str>,
        is_wildcard: bool,
    ) {
        let alias_id = alias.map(|s| self.intern(s));
        self.staging.add_edge(
            importer,
            imported,
            EdgeKind::Imports {
                alias: alias_id,
                is_wildcard,
            },
            self.file_id,
        );
    }

    /// Add an export edge from module to exported symbol.
    ///
    /// This method uses default metadata (`kind: ExportKind::Direct`, `alias: None`).
    /// Use [`add_export_edge_full`](Self::add_export_edge_full) for re-exports,
    /// default exports, namespace exports, or exports with aliases.
    pub fn add_export_edge(&mut self, module: NodeId, exported: NodeId) {
        self.staging.add_edge(
            module,
            exported,
            EdgeKind::Exports {
                kind: ExportKind::Direct,
                alias: None,
            },
            self.file_id,
        );
    }

    /// Add an export edge with full metadata.
    ///
    /// Use this method for re-exports, default exports, namespace exports,
    /// or exports with aliases. For simple direct exports without alias,
    /// use [`add_export_edge`](Self::add_export_edge).
    ///
    /// # Arguments
    ///
    /// * `module` - The module/file node that contains the export
    /// * `exported` - The symbol being exported
    /// * `kind` - The kind of export:
    ///   - `ExportKind::Direct` - Direct export (`export { foo }`)
    ///   - `ExportKind::Reexport` - Re-export from another module (`export { foo } from "mod"`)
    ///   - `ExportKind::Default` - Default export (`export default foo`)
    ///   - `ExportKind::Namespace` - Namespace export (`export * as ns from "mod"`)
    /// * `alias` - Optional alias string (e.g., for `export { foo as bar }`, alias is "bar")
    ///
    /// # Canonical Usage
    ///
    /// | Export Syntax (JS/TS) | Method |
    /// |-----------------------|--------|
    /// | `export { name }` | `add_export_edge(module, name)` |
    /// | `export default foo` | `add_export_edge_full(module, foo, ExportKind::Default, None)` |
    /// | `export { foo as bar }` | `add_export_edge_full(module, foo, ExportKind::Direct, Some("bar"))` |
    /// | `export { foo } from "mod"` | `add_export_edge_full(module, foo, ExportKind::Reexport, None)` |
    /// | `export { foo as bar } from "mod"` | `add_export_edge_full(module, foo, ExportKind::Reexport, Some("bar"))` |
    /// | `export * from "mod"` | `add_export_edge_full(module, mod, ExportKind::Reexport, None)` |
    /// | `export * as ns from "mod"` | `add_export_edge_full(module, mod, ExportKind::Namespace, Some("ns"))` |
    ///
    /// # Example
    ///
    /// ```ignore
    /// // export default MyComponent;
    /// helper.add_export_edge_full(module_id, component_id, ExportKind::Default, None);
    ///
    /// // export { helper as utilHelper };
    /// helper.add_export_edge_full(module_id, helper_id, ExportKind::Direct, Some("utilHelper"));
    ///
    /// // export * as utils from "./utils";
    /// helper.add_export_edge_full(module_id, utils_id, ExportKind::Namespace, Some("utils"));
    /// ```
    pub fn add_export_edge_full(
        &mut self,
        module: NodeId,
        exported: NodeId,
        kind: ExportKind,
        alias: Option<&str>,
    ) {
        let alias_id = alias.map(|s| self.intern(s));
        self.staging.add_edge(
            module,
            exported,
            EdgeKind::Exports {
                kind,
                alias: alias_id,
            },
            self.file_id,
        );
    }

    /// Add a reference edge (variable/field access).
    pub fn add_reference_edge(&mut self, from: NodeId, to: NodeId) {
        self.staging
            .add_edge(from, to, EdgeKind::References, self.file_id);
    }

    /// Add a defines edge (module defines symbol).
    pub fn add_defines_edge(&mut self, parent: NodeId, child: NodeId) {
        self.staging
            .add_edge(parent, child, EdgeKind::Defines, self.file_id);
    }

    /// Add a type-of edge (symbol has type).
    /// Add a `TypeOf` edge without context metadata (backward compatibility).
    ///
    /// For new code, prefer `add_typeof_edge_with_context` to provide semantic context.
    pub fn add_typeof_edge(&mut self, source: NodeId, target: NodeId) {
        self.add_typeof_edge_with_context(source, target, None, None, None);
    }

    /// Add a `TypeOf` edge with optional context metadata.
    ///
    /// # Parameters
    /// - `source`: The node that has this type (e.g., variable, function, parameter)
    /// - `target`: The type node
    /// - `context`: Where this type reference appears (Parameter, Return, Field, Variable, etc.)
    /// - `index`: Position/index (for parameters, returns, fields)
    /// - `name`: Name (for parameters, returns, fields, variables)
    ///
    /// # Examples
    /// ```ignore
    /// // Function parameter: func foo(ctx context.Context)
    /// helper.add_typeof_edge_with_context(
    ///     func_id,
    ///     type_id,
    ///     Some(TypeOfContext::Parameter),
    ///     Some(0),
    ///     Some("ctx"),
    /// );
    ///
    /// // Function return: func bar() error
    /// helper.add_typeof_edge_with_context(
    ///     func_id,
    ///     error_type_id,
    ///     Some(TypeOfContext::Return),
    ///     Some(0),
    ///     None,
    /// );
    ///
    /// // Variable: var x int
    /// helper.add_typeof_edge_with_context(
    ///     var_id,
    ///     int_type_id,
    ///     Some(TypeOfContext::Variable),
    ///     None,
    ///     Some("x"),
    /// );
    /// ```
    pub fn add_typeof_edge_with_context(
        &mut self,
        source: NodeId,
        target: NodeId,
        context: Option<TypeOfContext>,
        index: Option<u16>,
        name: Option<&str>,
    ) {
        let name_id = name.map(|n| self.intern(n));
        self.staging.add_edge(
            source,
            target,
            EdgeKind::TypeOf {
                context,
                index,
                name: name_id,
            },
            self.file_id,
        );
    }

    /// Add an implements edge (class implements interface).
    pub fn add_implements_edge(&mut self, implementor: NodeId, interface: NodeId) {
        self.staging
            .add_edge(implementor, interface, EdgeKind::Implements, self.file_id);
    }

    /// Add an inherits edge (class extends class).
    pub fn add_inherits_edge(&mut self, child: NodeId, parent: NodeId) {
        self.staging
            .add_edge(child, parent, EdgeKind::Inherits, self.file_id);
    }

    /// Add a contains edge (parent contains child, e.g., class contains method).
    pub fn add_contains_edge(&mut self, parent: NodeId, child: NodeId) {
        self.staging
            .add_edge(parent, child, EdgeKind::Contains, self.file_id);
    }

    /// Add a WebAssembly call edge.
    ///
    /// Used when JavaScript/TypeScript code instantiates or calls WebAssembly modules:
    /// - `WebAssembly.instantiate()` / `WebAssembly.instantiateStreaming()`
    /// - `new WebAssembly.Module()` / `new WebAssembly.Instance()`
    /// - Calling exported WASM functions
    pub fn add_webassembly_edge(&mut self, caller: NodeId, wasm_target: NodeId) {
        self.staging
            .add_edge(caller, wasm_target, EdgeKind::WebAssemblyCall, self.file_id);
    }

    /// Add an FFI call edge with the specified calling convention.
    ///
    /// Used for foreign function interface calls:
    /// - Node.js native addons (`.node` files)
    /// - ctypes/cffi in Python
    /// - JNI in Java
    /// - P/Invoke in C#
    pub fn add_ffi_edge(&mut self, caller: NodeId, ffi_target: NodeId, convention: FfiConvention) {
        self.staging.add_edge(
            caller,
            ffi_target,
            EdgeKind::FfiCall { convention },
            self.file_id,
        );
    }

    /// Add an HTTP request edge.
    ///
    /// Use this when detecting HTTP calls like `fetch()` or `axios.get()`.
    pub fn add_http_request_edge(
        &mut self,
        caller: NodeId,
        target: NodeId,
        method: HttpMethod,
        url: Option<&str>,
    ) {
        let url_id = url.map(|value| self.intern(value));
        self.staging.add_edge(
            caller,
            target,
            EdgeKind::HttpRequest {
                method,
                url: url_id,
            },
            self.file_id,
        );
    }

    /// Ensure a function node exists, creating it if needed.
    ///
    /// This is a convenience method that matches the legacy `ensure_function_node` pattern.
    pub fn ensure_function(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_unsafe: bool,
    ) -> NodeId {
        self.add_function(qualified_name, span, is_async, is_unsafe)
    }

    /// Ensure a method node exists, creating it if needed.
    pub fn ensure_method(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_static: bool,
    ) -> NodeId {
        self.add_method(qualified_name, span, is_async, is_static)
    }

    /// Get statistics about what's been staged.
    #[must_use]
    pub fn stats(&self) -> HelperStats {
        let staging_stats = self.staging.stats();
        HelperStats {
            strings_interned: self.string_cache.len(),
            nodes_created: self.node_cache.len(),
            nodes_staged: staging_stats.nodes_staged,
            edges_staged: staging_stats.edges_staged,
        }
    }
}

/// Statistics from `GraphBuildHelper` operations.
#[derive(Debug, Clone, Default)]
pub struct HelperStats {
    /// Number of unique strings interned.
    pub strings_interned: usize,
    /// Number of unique nodes created.
    pub nodes_created: usize,
    /// Total nodes staged (from `StagingGraph`).
    pub nodes_staged: usize,
    /// Total edges staged (from `StagingGraph`).
    pub edges_staged: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Position;
    use crate::graph::unified::build::staging::StagingOp;
    use std::path::PathBuf;

    #[test]
    fn test_helper_add_function() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let node_id = helper.add_function("main", None, false, false);
        assert!(!node_id.is_invalid());
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_helper_deduplication() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let id1 = helper.add_function("main", None, false, false);
        let id2 = helper.add_function("main", None, false, false);

        assert_eq!(id1, id2, "Same function should return same NodeId");
        assert_eq!(
            helper.stats().nodes_created,
            1,
            "Should only create one node"
        );
    }

    #[test]
    fn test_helper_string_interning() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let s1 = helper.intern("hello");
        let s2 = helper.intern("world");
        let s3 = helper.intern("hello"); // Duplicate

        assert_ne!(s1, s2, "Different strings should have different IDs");
        assert_eq!(s1, s3, "Same string should return same ID");
        assert_eq!(helper.stats().strings_interned, 2);
    }

    #[test]
    fn test_helper_add_call_edge() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let main_id = helper.add_function("main", None, false, false);
        let helper_id = helper.add_function("helper", None, false, false);

        helper.add_call_edge(main_id, helper_id);

        assert_eq!(helper.stats().edges_staged, 1);
        let edge_kind = staging.operations().iter().find_map(|op| {
            if let StagingOp::AddEdge { kind, .. } = op {
                Some(kind)
            } else {
                None
            }
        });
        match edge_kind {
            Some(EdgeKind::Calls {
                argument_count,
                is_async,
            }) => {
                assert_eq!(*argument_count, 255);
                assert!(!*is_async);
            }
            _ => panic!("Expected Calls edge"),
        }
    }

    #[test]
    fn test_helper_multiple_node_kinds() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.py");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Python);

        let _class_id = helper.add_class("MyClass", None);
        let _method_id = helper.add_method("MyClass.my_method", None, false, false);
        let _func_id = helper.add_function("standalone_func", None, true, false);

        assert_eq!(helper.stats().nodes_created, 3);
    }

    #[test]
    fn test_helper_ensure_function() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let id1 = helper.ensure_function("foo", None, false, false);
        let id2 = helper.ensure_function("foo", None, true, false); // Different attrs, same name

        assert_eq!(id1, id2, "ensure_function should be idempotent by name");
    }

    #[test]
    fn test_helper_with_span() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let span = Span {
            start: Position {
                line: 10,
                column: 0,
            },
            end: Position {
                line: 15,
                column: 1,
            },
        };

        let node_id = helper.add_function("main", Some(span), false, false);
        assert!(!node_id.is_invalid());
    }

    #[test]
    fn test_helper_add_call_edge_full() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let caller_id = helper.add_function("caller", None, false, false);
        let callee_id = helper.add_function("callee", None, false, false);

        // Add a call with specific metadata
        helper.add_call_edge_full(caller_id, callee_id, 3, true);

        assert_eq!(helper.stats().edges_staged, 1);

        // Verify the edge has correct metadata
        let edges = staging.operations();
        let call_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Calls { .. },
                    ..
                }
            )
        });

        assert!(call_edge.is_some());
        if let StagingOp::AddEdge {
            kind:
                EdgeKind::Calls {
                    argument_count,
                    is_async,
                },
            ..
        } = call_edge.unwrap()
        {
            assert_eq!(*argument_count, 3);
            assert!(*is_async);
        }
    }

    #[test]
    fn test_helper_add_import_edge_full() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.js");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::JavaScript);

        let module_id = helper.add_module("app", None);
        let imported_id = helper.add_function("utils", None, false, false);

        // Import with alias
        helper.add_import_edge_full(module_id, imported_id, Some("helpers"), false);

        assert_eq!(helper.stats().edges_staged, 1);

        // Verify the edge has correct metadata
        let edges = staging.operations();
        let import_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });

        assert!(import_edge.is_some());
        if let StagingOp::AddEdge {
            kind: EdgeKind::Imports { alias, is_wildcard },
            ..
        } = import_edge.unwrap()
        {
            assert!(alias.is_some(), "Alias should be present");
            assert!(!*is_wildcard);
        }
    }

    #[test]
    fn test_helper_add_import_edge_wildcard() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.js");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::JavaScript);

        let module_id = helper.add_module("app", None);
        let imported_id = helper.add_module("lodash", None);

        // Wildcard import: import * from "lodash"
        helper.add_import_edge_full(module_id, imported_id, None, true);

        let edges = staging.operations();
        let import_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Imports { .. },
                    ..
                }
            )
        });

        if let StagingOp::AddEdge {
            kind: EdgeKind::Imports { alias, is_wildcard },
            ..
        } = import_edge.unwrap()
        {
            assert!(alias.is_none());
            assert!(*is_wildcard);
        }
    }

    #[test]
    fn test_helper_add_export_edge_full() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.js");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::JavaScript);

        let module_id = helper.add_module("app", None);
        let component_id = helper.add_class("MyComponent", None);

        // Default export
        helper.add_export_edge_full(module_id, component_id, ExportKind::Default, None);

        assert_eq!(helper.stats().edges_staged, 1);

        let edges = staging.operations();
        let export_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        });

        assert!(export_edge.is_some());
        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, alias },
            ..
        } = export_edge.unwrap()
        {
            assert_eq!(*kind, ExportKind::Default);
            assert!(alias.is_none());
        }
    }

    #[test]
    fn test_helper_add_export_edge_with_alias() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.js");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::JavaScript);

        let module_id = helper.add_module("app", None);
        let helper_fn_id = helper.add_function("internalHelper", None, false, false);

        // export { internalHelper as helper }
        helper.add_export_edge_full(module_id, helper_fn_id, ExportKind::Direct, Some("helper"));

        let edges = staging.operations();
        let export_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        });

        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, alias },
            ..
        } = export_edge.unwrap()
        {
            assert_eq!(*kind, ExportKind::Direct);
            assert!(alias.is_some(), "Alias should be present");
        }
    }

    #[test]
    fn test_helper_add_export_edge_reexport() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("index.js");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::JavaScript);

        let module_id = helper.add_module("index", None);
        let utils_id = helper.add_module("utils", None);

        // export * as utils from "./utils"
        helper.add_export_edge_full(module_id, utils_id, ExportKind::Namespace, Some("utils"));

        let edges = staging.operations();
        let export_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Exports { .. },
                    ..
                }
            )
        });

        if let StagingOp::AddEdge {
            kind: EdgeKind::Exports { kind, alias },
            ..
        } = export_edge.unwrap()
        {
            assert_eq!(*kind, ExportKind::Namespace);
            assert!(alias.is_some());
        }
    }

    #[test]
    fn test_helper_add_call_edge_full_with_span() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let caller_id = helper.add_function("caller", None, false, false);
        let callee_id = helper.add_function("callee", None, false, false);

        let span = Span {
            start: Position { line: 5, column: 4 },
            end: Position {
                line: 5,
                column: 20,
            },
        };

        helper.add_call_edge_full_with_span(caller_id, callee_id, 2, false, vec![span]);

        let edges = staging.operations();
        let call_edge = edges.iter().find(|op| {
            matches!(
                op,
                StagingOp::AddEdge {
                    kind: EdgeKind::Calls { .. },
                    ..
                }
            )
        });

        if let StagingOp::AddEdge {
            kind:
                EdgeKind::Calls {
                    argument_count,
                    is_async,
                },
            spans: edge_spans,
            ..
        } = call_edge.unwrap()
        {
            assert_eq!(*argument_count, 2);
            assert!(!*is_async);
            assert!(!edge_spans.is_empty());
        }
    }

    #[test]
    fn test_helper_add_function_with_async_attribute() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.kt");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Kotlin);

        // Add an async (suspend) function
        let _func_id = helper.add_function("fetchData", None, true, false);

        // Verify the staged node has is_async = true
        let ops = staging.operations();
        let add_node_op = ops
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }));

        assert!(add_node_op.is_some(), "Expected AddNode operation");
        if let StagingOp::AddNode { entry, .. } = add_node_op.unwrap() {
            assert!(
                entry.is_async,
                "Expected is_async=true for suspend function, got is_async=false"
            );
        }
    }

    #[test]
    fn test_helper_add_method_with_static_attribute() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.java");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Java);

        // Add a static method
        let _method_id = helper.add_method("MyClass.staticMethod", None, false, true);

        // Verify the staged node has is_static = true
        let ops = staging.operations();
        let add_node_op = ops
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }));

        assert!(add_node_op.is_some(), "Expected AddNode operation");
        if let StagingOp::AddNode { entry, .. } = add_node_op.unwrap() {
            assert!(
                entry.is_static,
                "Expected is_static=true for static method, got is_static=false"
            );
        }
    }

    #[test]
    fn test_helper_add_function_without_attributes() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        // Add a regular function (not async, not unsafe)
        let _func_id = helper.add_function("regular_function", None, false, false);

        // Verify the staged node has is_async = false
        let ops = staging.operations();
        let add_node_op = ops
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }));

        assert!(add_node_op.is_some(), "Expected AddNode operation");
        if let StagingOp::AddNode { entry, .. } = add_node_op.unwrap() {
            assert!(
                !entry.is_async,
                "Expected is_async=false for regular function"
            );
            assert!(
                !entry.is_static,
                "Expected is_static=false for regular function"
            );
        }
    }

    #[test]
    fn test_helper_add_method_with_both_attributes() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.kt");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Kotlin);

        // Add an async static method
        let _method_id = helper.add_method("Service.asyncStaticMethod", None, true, true);

        // Verify the staged node has both flags set
        let ops = staging.operations();
        let add_node_op = ops
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }));

        assert!(add_node_op.is_some(), "Expected AddNode operation");
        if let StagingOp::AddNode { entry, .. } = add_node_op.unwrap() {
            assert!(entry.is_async, "Expected is_async=true for async method");
            assert!(entry.is_static, "Expected is_static=true for static method");
        }
    }

    #[test]
    fn test_helper_add_function_with_unsafe_attribute() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        // Add an unsafe function
        let _func_id = helper.add_function("unsafe_function", None, false, true);

        // Verify the staged node has is_unsafe = true
        let ops = staging.operations();
        let add_node_op = ops
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }));

        assert!(add_node_op.is_some(), "Expected AddNode operation");
        if let StagingOp::AddNode { entry, .. } = add_node_op.unwrap() {
            assert!(
                entry.is_unsafe,
                "Expected is_unsafe=true for unsafe function, got is_unsafe={}",
                entry.is_unsafe
            );
        }
    }
}
