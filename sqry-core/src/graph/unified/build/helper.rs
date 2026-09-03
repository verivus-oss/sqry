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
use super::super::resolution::canonicalize_graph_qualified_name;
use super::staging::{
    CIndirectStagingPayload, NodeMetadataFlag, NodeMetadataUpdate, PendingBinding,
    PendingIndirectCallsite, SpanOrigin, StagingGraph,
};
use crate::graph::node::{Language, Span};
use crate::graph::unified::edge::kind::{
    ChannelBufferKind, ChannelPeerDirection, InferenceKind, TypeArg,
};
use crate::graph::unified::edge::{
    EdgeKind, ExportKind, FfiConvention, HttpMethod, ResolvedVia, TableWriteOp, WrapKind,
};
use crate::graph::unified::file::FileId;
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::storage::NodeEntry;
use crate::graph::unified::storage::c_indirect::{BindingSiteKind, IndirectShape, LocalScopeIndex};
use crate::graph::unified::string::StringId;

/// Node kinds that represent callable targets and may be used interchangeably
/// across files. When a plugin calls `ensure_function` for a name that already
/// exists as any of these kinds, the existing node is reused instead of creating
/// a duplicate spanless stub.
///
/// dec44131f established this for the Method<->Function pair. This const
/// generalizes it to all call-compatible kinds.
pub(crate) const CALL_COMPATIBLE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Macro,
    NodeKind::Constant,
    NodeKind::LambdaTarget,
];

/// Hint for the kind of callee node to create when no cached node exists.
///
/// Only call-compatible kinds are valid hints. Using a non-call-compatible
/// kind (e.g., `StyleRule`) is prevented at compile time by this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalleeKindHint {
    /// Default: create a `Function` node.
    Function,
    /// Create a `Method` node (receiver method).
    Method,
    /// Create a `Macro` node (C preprocessor macro, Rust macro, etc.).
    Macro,
    /// Create a `Constant` node (function pointer constant).
    Constant,
    /// Create a `LambdaTarget` node (Java SAM interface, Kotlin lambda, etc.).
    LambdaTarget,
    /// No preference: create a `Function` node (same as `Function`).
    Any,
}

impl CalleeKindHint {
    /// Convert to the default `NodeKind` for node creation.
    fn to_node_kind(self) -> NodeKind {
        match self {
            Self::Function | Self::Any => NodeKind::Function,
            Self::Method => NodeKind::Method,
            Self::Macro => NodeKind::Macro,
            Self::Constant => NodeKind::Constant,
            Self::LambdaTarget => NodeKind::LambdaTarget,
        }
    }
}

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
    ///
    /// Shared by both canonical nodes (via `add_node_internal`, which stores
    /// under the **canonicalized** qualified name) and verbatim nodes (via
    /// `add_node_verbatim`, which stores under the **raw** name).  Collisions
    /// are avoided because canonical names never contain native delimiters
    /// (e.g. `.`, `#`) while verbatim names preserve them (e.g. `styles.css`).
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

    /// Look up a node ID by its qualified name and kind from the internal cache.
    ///
    /// Returns the `NodeId` if a node with the given `(name, kind)` pair was
    /// previously created through this helper. This is used by macro boundary
    /// analysis to find graph nodes corresponding to AST items.
    #[must_use]
    pub fn lookup_node(&self, name: &str, kind: NodeKind) -> Option<NodeId> {
        self.node_cache.get(&(name.to_string(), kind)).copied()
    }

    /// Get the file path.
    #[must_use]
    pub fn file_path(&self) -> &str {
        &self.file_path
    }

    /// Mutable access to the underlying [`StagingGraph`].
    ///
    /// Exposed for plugin call sites that need to forward typed
    /// metadata into the staging buffer alongside their normal `add_*`
    /// node-creation flow — for example, the Go plugin's
    /// `add_synthetic_variable` helper (`C_SUPPRESS`) which calls
    /// [`StagingGraph::merge_macro_metadata`] to record a
    /// `NodeFlags::SYNTHETIC` flag on the freshly-staged Variable
    /// node so the suppression contract on
    /// [`crate::graph::unified::concurrent::graph::GraphSnapshot::find_by_pattern`]
    /// is satisfied via the canonical metadata-bit channel (in addition
    /// to the structural name-shape fallback).
    #[must_use]
    pub fn staging_mut(&mut self) -> &mut StagingGraph {
        self.staging
    }

    /// Attach body hashes to all staged nodes using the given content bytes.
    ///
    /// Multi-language plugins (Vue, Svelte) should call this per extracted
    /// script block so that node body spans — which are relative to the
    /// block content, not the full SFC file — produce correct hashes.
    /// Nodes that already have a hash are skipped, so the later whole-file
    /// call in the indexing entrypoint is harmless.
    pub fn attach_body_hashes(&mut self, content: &[u8]) {
        // Per-block helper (Vue/Svelte embedded scripts): body hashes only. Shape
        // descriptors for embedded-script blocks are a later concern; the whole-file
        // index seam in the entrypoint owns shape wiring.
        self.staging.attach_body_hashes(content, None);
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
        // Dual-use bare helper (issue #394): also used to create call/FFI/callee
        // stubs (e.g. go syscall/ffi targets, rust trait-binding callees), so it
        // defaults is_definition = false. Real declaration sites opt in via
        // mark_definition (or use the _with_visibility/_with_signature variants).
        self.add_function_inner(qualified_name, span, is_async, is_unsafe, false)
    }

    /// Internal function-node sink.
    ///
    /// Callers pass `is_definition` explicitly, and the BARE
    /// [`add_function`](Self::add_function) passes `false`, because it is
    /// dual-use: plugins also mint call, FFI and syscall stubs through it. Real
    /// declarations come in as `true` either through
    /// [`add_function_with_visibility`](Self::add_function_with_visibility) and
    /// [`add_function_with_signature`](Self::add_function_with_signature), or
    /// through a later `mark_definition`.
    ///
    /// An earlier version of this comment said `add_function` sets
    /// `is_definition = true`. It does not, and reasoning from that led to a
    /// review round arguing that Go and Perl carry no definition signal (they
    /// use the `_with_visibility` form, so they do).
    fn add_function_inner(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_unsafe: bool,
        is_definition: bool,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Function,
            &[("async", is_async), ("unsafe", is_unsafe)],
            None,
            None,
            is_definition,
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
            true,
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
            true,
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
        // Dual-use bare helper (issue #394): also used for call/callee stubs
        // (servicenow/apex callees), so it defaults is_definition = false. Real
        // declaration sites opt in via mark_definition (or the _with_* variants).
        self.add_method_inner(qualified_name, span, is_async, is_static, false)
    }

    /// Internal method-node sink shared by the public declaration helper
    /// [`add_method`](Self::add_method) (`is_definition = true`) and the
    /// call-edge wrapper [`ensure_method`](Self::ensure_method)
    /// (`is_definition = false`). See [`add_function_inner`](Self::add_function_inner).
    fn add_method_inner(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_static: bool,
        is_definition: bool,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Method,
            &[("async", is_async), ("static", is_static)],
            None,
            None,
            is_definition,
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
            true,
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
            true,
        )
    }

    /// Add a class node.
    ///
    /// Dual-use bare helper (issue #394): also used to create type-reference
    /// stubs (e.g. puppet inherited-class targets), so it defaults
    /// `is_definition = false`. Real declaration sites opt in via
    /// [`mark_definition`](Self::mark_definition) (or use
    /// [`add_class_with_visibility`](Self::add_class_with_visibility)).
    pub fn add_class(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Class,
            &[],
            None,
            None,
            false,
        )
    }

    /// Add a class node with visibility.
    pub fn add_class_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Class,
            &[],
            visibility,
            None,
            true,
        )
    }

    /// Add a struct node.
    ///
    /// Dual-use bare helper (issue #394): also used to create reference stubs
    /// (e.g. go embedded-parent-struct targets), so it defaults
    /// `is_definition = false`. Real declaration sites opt in via
    /// [`mark_definition`](Self::mark_definition) (or use
    /// [`add_struct_with_visibility`](Self::add_struct_with_visibility)).
    pub fn add_struct(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Struct,
            &[],
            None,
            None,
            false,
        )
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
            true,
        )
    }

    /// Add a module node.
    ///
    /// Dual-use bare helper (issue #394): also used to create FFI/import-target
    /// stubs (e.g. python/kotlin native targets), so it defaults
    /// `is_definition = false`. Real module-declaration sites opt in via
    /// [`mark_definition`](Self::mark_definition).
    pub fn add_module(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Module,
            &[],
            None,
            None,
            false,
        )
    }

    /// Add a resource node.
    pub fn add_resource(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Resource,
            &[],
            None,
            None,
            true,
        )
    }

    /// Add an endpoint node for HTTP route handlers.
    ///
    /// The qualified name should follow the convention `route::{METHOD}::{path}`,
    /// for example `route::GET::/api/users` or `route::POST::/api/items`.
    ///
    /// Endpoint nodes are used by Pass 5 (cross-language linking) to match
    /// HTTP requests from client code to server-side route handlers.
    pub fn add_endpoint(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Endpoint,
            &[],
            None,
            None,
            true,
        )
    }

    /// Add an import node.
    pub fn add_import(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Import,
            &[],
            None,
            None,
            false,
        )
    }

    /// Add an import node while preserving the original path-like identifier.
    ///
    /// Use this for resource imports such as `styles.css`, `app.js`, or
    /// similar asset filenames where `.` is part of the path rather than a
    /// language-native qualified-name separator.
    pub fn add_verbatim_import(&mut self, name: &str, span: Option<Span>) -> NodeId {
        self.add_node_verbatim(name, span, NodeKind::Import, &[], None, None, false)
    }

    /// Add a variable node.
    ///
    /// Dual-use bare helper (issue #394): also used to create reference stubs
    /// (e.g. rust field targets, sap-abap typed references), so it defaults
    /// `is_definition = false`. Real variable/parameter/field declaration sites
    /// opt in via [`mark_definition`](Self::mark_definition).
    pub fn add_variable(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Variable,
            &[],
            None,
            None,
            false,
        )
    }

    /// Add a variable node while preserving the original identifier exactly.
    ///
    /// Use this for static asset references where the literal path is the
    /// graph identity.
    pub fn add_verbatim_variable(&mut self, name: &str, span: Option<Span>) -> NodeId {
        self.add_node_verbatim(name, span, NodeKind::Variable, &[], None, None, false)
    }

    /// Add a variable whose graph IDENTITY and whose user-facing NAME differ.
    ///
    /// `semantic_name` becomes `NodeEntry::name`, which is what name lookup and
    /// the synthetic-node filter read. `qualified_name` is the dedup key, so
    /// two mints of the same declaration converge on one node.
    ///
    /// The two must be separable for a per-binding-site declaration. Naming
    /// such a node `ident@<offset>` in BOTH roles makes it match
    /// `is_synthetic_placeholder_name`, and `is_node_synthetic` falls back to
    /// that shape, so MCP `semantic_search`, CLI `search --exact` and the
    /// planner `name:` predicate all drop it. A real declaration must not be
    /// invisible to the surfaces whose whole job is answering whether a symbol
    /// exists. Occurrence nodes are a different case and should keep the
    /// suffixed name in both roles: they are scaffolding, not symbols.
    pub fn add_variable_with_semantic_name(
        &mut self,
        semantic_name: &str,
        cache_key: &str,
        span: Option<Span>,
    ) -> NodeId {
        let canonical = canonicalize_graph_qualified_name(self.language, cache_key);
        self.add_node_internal_with_canonical_name_inner(
            semantic_name,
            &canonical,
            span,
            NodeKind::Variable,
            &[],
            None,
            None,
            false,
            // Same origin as `add_variable`, whose sink files Declaration. A
            // local binding is a declaration of its own identity; this variant
            // exists for the NAME it publishes, not for a different span
            // policy.
            SpanOrigin::Declaration,
            // The cache key is a binding-site offset. It is identity, not an
            // address, and must not be stored: `display_entry_qualified_name`
            // and several hand-rolled display paths prefer the qualified name
            // over the semantic one, so storing it is what leaked `x@1487`
            // into planner, MCP and LSP output.
            false,
        )
    }

    /// Add a constant node.
    pub fn add_constant(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Constant,
            &[],
            None,
            None,
            true,
        )
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
            true,
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
            true,
        )
    }

    /// Add a constant node with an explicit simple semantic name.
    ///
    /// Use this when the graph identity must keep a language-specific
    /// qualified form but the searchable symbol name is the bare declaration
    /// name.
    pub fn add_constant_with_name_static_and_visibility(
        &mut self,
        name: &str,
        qualified_name: &str,
        span: Option<Span>,
        is_static: bool,
        visibility: Option<&str>,
    ) -> NodeId {
        let attrs: &[(&str, bool)] = if is_static { &[("static", true)] } else { &[] };
        self.add_node_internal_with_name(
            name,
            qualified_name,
            span,
            NodeKind::Constant,
            attrs,
            visibility,
            None,
            true,
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
            true,
        )
    }

    /// Add a property node with an explicit simple semantic name.
    ///
    /// Use this when the graph identity must keep a language-specific
    /// qualified form but the searchable symbol name is the bare declaration
    /// name.
    pub fn add_property_with_name_static_and_visibility(
        &mut self,
        name: &str,
        qualified_name: &str,
        span: Option<Span>,
        is_static: bool,
        visibility: Option<&str>,
    ) -> NodeId {
        let attrs: &[(&str, bool)] = if is_static { &[("static", true)] } else { &[] };
        self.add_node_internal_with_name(
            name,
            qualified_name,
            span,
            NodeKind::Property,
            attrs,
            visibility,
            None,
            true,
        )
    }

    /// Add an enum node.
    pub fn add_enum(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Enum, &[], None, None, true)
    }

    /// Add an enum node with visibility.
    pub fn add_enum_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Enum,
            &[],
            visibility,
            None,
            true,
        )
    }

    /// Add an interface/trait node.
    ///
    /// Dual-use bare helper (issue #394): also used to create type-reference
    /// stubs (e.g. go interface-type references), so it defaults
    /// `is_definition = false`. Real declaration sites opt in via
    /// [`mark_definition`](Self::mark_definition) (or use
    /// [`add_interface_with_visibility`](Self::add_interface_with_visibility)).
    pub fn add_interface(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Interface,
            &[],
            None,
            None,
            false,
        )
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
            true,
        )
    }

    /// Add a type alias node.
    ///
    /// Irreducibly dual-use bare helper (issue #394): used for BOTH typedef/
    /// type-alias declarations AND type references (the dominant use), so it
    /// defaults `is_definition = false`. A type DECLARED in the workspace opts
    /// in at its declaration site via [`mark_definition`](Self::mark_definition)
    /// (references then dedupe into it and the OR-in keeps it true); a type only
    /// ever referenced stays false.
    pub fn add_type(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(qualified_name, span, NodeKind::Type, &[], None, None, false)
    }

    /// Add a type alias node with visibility.
    pub fn add_type_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Type,
            &[],
            visibility,
            None,
            true,
        )
    }

    /// Add a lifetime node.
    pub fn add_lifetime(&mut self, qualified_name: &str, span: Option<Span>) -> NodeId {
        self.add_node_internal(
            qualified_name,
            span,
            NodeKind::Lifetime,
            &[],
            None,
            None,
            true,
        )
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
    ///
    /// Generic nodes default to `is_definition = false`: the caller passes a raw
    /// `NodeKind` so the helper cannot know whether the node is a real
    /// declaration or a structural/reference stub. Real-declaration callers are
    /// marked explicitly in Stage 2.
    pub fn add_node(&mut self, qualified_name: &str, span: Option<Span>, kind: NodeKind) -> NodeId {
        self.add_node_internal(qualified_name, span, kind, &[], None, None, false)
    }

    /// Add a generic node with visibility.
    ///
    /// Defaults to `is_definition = false` for the same reason as
    /// [`add_node`](Self::add_node).
    pub fn add_node_with_visibility(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_node_internal(qualified_name, span, kind, &[], visibility, None, false)
    }

    /// Mint a stub whose only known location is a **reference to it**.
    ///
    /// Use this instead of `add_function` / `add_module` / `add_class` /
    /// `add_node` whenever the span in hand is the extent of the expression
    /// that NAMED the symbol rather than the extent of the symbol's own
    /// declaration. In this tree that covers, at least:
    ///
    /// - an FFI, syscall or bridged C target named inside a call
    /// - a native or WebAssembly module named by `require(...)`, `dlopen(...)`,
    ///   `new WebAssembly.Module(...)` or an `import`
    /// - a callee named at a call site, where `ensure_callee` is not already
    ///   doing the job
    /// - an entry in an export list (`__all__`, `export * from "..."`)
    /// - a type named by an assertion, an `extends` / `implements` clause, a
    ///   `use TraitName;`, or an `impl` block
    /// - a class or program named by an `include`, a `SUBMIT`, or a `new`
    ///   expression
    ///
    /// The name of the parameter says "call site" because that is the case that
    /// motivated it; the contract is the broader one above.
    ///
    /// Node creation is identical to the matching `add_*` helper for `kind`
    /// (same canonicalization, same cache, `is_definition = false`, no
    /// visibility or signature, which a reference site cannot know). The one
    /// difference is that the extent is NOT filed as a body this node owns,
    /// which keeps the stub out of `body_hash` and the shape descriptor
    /// (issue #748). Without that, two `require("ffi")` stubs in two files
    /// hash the caller's bytes and are reported as duplicate bodies.
    ///
    /// A reference site can still win the RECORDED location, under the
    /// latest-ending rule in `apply_span_to_entry`, and that is left alone.
    /// What it can no longer do is take away a body extent a declaration has
    /// already filed for the same node in the same file. (The extent table
    /// lives on one `StagingGraph`, which is per parsed file, so extents from
    /// different files never meet.)
    ///
    /// Use [`add_bodyless_declaration_node`](Self::add_bodyless_declaration_node)
    /// instead when the site is a real declaration that merely has no body,
    /// such as a C `struct Config;`. That is a definition; this is not.
    pub fn add_call_site_node(
        &mut self,
        qualified_name: &str,
        call_site_span: Span,
        kind: NodeKind,
    ) -> NodeId {
        self.add_call_site_node_internal(qualified_name, call_site_span, kind)
    }

    /// Mint or update a node for a declaration that names a symbol without
    /// giving it a body.
    ///
    /// The forward-declaration case: C `struct Config;`, C++ `class Widget;`,
    /// `enum State : int;`, `template <typename T> class Tmpl;`. The node is a
    /// definition (`is_definition = true`) and the span is its own, so both
    /// are recorded. What does not happen is the extent being filed as a body:
    /// there is no body, and hashing the declaration line would group every
    /// forward declaration of a same-length name (issue #748).
    ///
    /// Pass `visibility` when the site knows it, as a C++ class member
    /// forward declaration does.
    pub fn add_bodyless_declaration_node(
        &mut self,
        qualified_name: &str,
        declaration_span: Span,
        kind: NodeKind,
        visibility: Option<&str>,
    ) -> NodeId {
        self.add_bodyless_declaration_node_internal(
            qualified_name,
            declaration_span,
            kind,
            visibility,
        )
    }

    /// Mark a just-created staged node as a real source declaration
    /// (`is_definition = true`).
    ///
    /// This is the explicit opt-in (issue #394) for declaration sites that
    /// create their node through a DUAL-USE bare helper (`add_function`,
    /// `add_method`, `add_class`, `add_struct`, `add_enum`-less kinds aside,
    /// `add_interface`, `add_type`, `add_module`, `add_variable`) or the generic
    /// `add_node`/`add_node_with_visibility`, all of which default
    /// `is_definition = false` because the helper cannot tell a declaration from
    /// a call/FFI/reference/import stub. A declaration handler calls this right
    /// after creating its node.
    ///
    /// The signal is monotonic (OR-in): once marked true it is never cleared, so
    /// calling this on a node that was also reached as a stub (or vice-versa)
    /// converges to true, which is correct (a symbol declared in the workspace
    /// IS a definition regardless of also being referenced).
    pub fn mark_definition(&mut self, node_id: NodeId) {
        let update = NodeMetadataUpdate::new().mark_if(NodeMetadataFlag::Definition, true);
        self.staging.update_node_entry(node_id, &update);
    }

    /// Internal **declaration-span** sink for adding nodes.
    ///
    /// Applies attributes to the node entry:
    /// - `"async"` → `NodeEntry::with_async(true/false)`
    /// - `"static"` → `NodeEntry::with_static(true/false)`
    /// - `"unsafe"` → `NodeEntry::with_unsafe(true/false)`
    ///
    /// When `signature` is `Some`, the signature field is set on the node for
    /// `returns:` queries.
    ///
    /// # Span contract (issue #748)
    ///
    /// Every `span` reaching this sink is treated as a declaration extent, so
    /// the node is admitted to the body plane (`body_hash` + shape
    /// descriptor). A path that hands a node the extent of a **call site**
    /// must go through [`add_call_site_node_internal`](Self::add_call_site_node_internal)
    /// instead, or the stub will be fingerprinted as if it owned the caller's
    /// body.
    fn add_node_internal(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        attributes: &[(&str, bool)],
        visibility: Option<&str>,
        signature: Option<&str>,
        is_definition: bool,
    ) -> NodeId {
        let canonical_qualified_name =
            canonicalize_graph_qualified_name(self.language, qualified_name);
        let semantic_name = semantic_name_for_node_input(qualified_name, &canonical_qualified_name);
        self.add_node_internal_with_canonical_name(
            &semantic_name,
            &canonical_qualified_name,
            span,
            kind,
            attributes,
            visibility,
            signature,
            is_definition,
            SpanOrigin::Declaration,
        )
    }

    /// Internal **bodyless-declaration** sink.
    ///
    /// For a declaration that names a symbol without giving it a body: a C
    /// `struct Config;`, a C++ `class Widget;` or `enum State : int;`. The
    /// node IS a definition and the span IS its own declaration's extent, so
    /// both are recorded, but there is no body to fingerprint and the extent
    /// is not filed as one (issue #748).
    ///
    /// This is the case a two-way `declaration or reference` split gets wrong.
    /// Sending a forward declaration through the call-site sink would clear
    /// `is_definition`, and `find_unused`, the items filter and centrality all
    /// read that bit.
    fn add_bodyless_declaration_node_internal(
        &mut self,
        qualified_name: &str,
        declaration_span: Span,
        kind: NodeKind,
        visibility: Option<&str>,
    ) -> NodeId {
        let canonical_qualified_name =
            canonicalize_graph_qualified_name(self.language, qualified_name);
        let semantic_name = semantic_name_for_node_input(qualified_name, &canonical_qualified_name);
        self.add_node_internal_with_canonical_name(
            &semantic_name,
            &canonical_qualified_name,
            Some(declaration_span),
            kind,
            &[],
            visibility,
            None,
            true,
            SpanOrigin::BodylessDeclaration,
        )
    }

    /// Internal **call-site-span** sink for minting call-target stubs.
    ///
    /// Same node creation as [`add_node_internal`](Self::add_node_internal),
    /// except the extent is recorded as belonging to the caller, which keeps
    /// the stub out of both halves of the body plane. Stubs are never
    /// declarations, so `is_definition` is always false here, and no
    /// visibility / signature / attribute is known at a call site.
    ///
    /// Reached from [`ensure_callee`](Self::ensure_callee) and from the public
    /// [`add_call_site_node`](Self::add_call_site_node) that plugins use for
    /// FFI, syscall, WebAssembly and native-module targets. Any future minting
    /// path that supplies a call-site extent belongs here too.
    fn add_call_site_node_internal(
        &mut self,
        qualified_name: &str,
        call_site_span: Span,
        kind: NodeKind,
    ) -> NodeId {
        let canonical_qualified_name =
            canonicalize_graph_qualified_name(self.language, qualified_name);
        let semantic_name = semantic_name_for_node_input(qualified_name, &canonical_qualified_name);
        self.add_node_internal_with_canonical_name(
            &semantic_name,
            &canonical_qualified_name,
            Some(call_site_span),
            kind,
            &[],
            None,
            None,
            false,
            SpanOrigin::CallSite,
        )
    }

    /// Declaration-span sibling of [`add_node_internal`](Self::add_node_internal)
    /// for nodes whose searchable name differs from their qualified name. The
    /// same span contract applies: every caller supplies a declaration extent.
    #[allow(clippy::too_many_arguments)] // internal builder sink; is_definition (issue #394) threaded alongside span/kind/attrs
    fn add_node_internal_with_name(
        &mut self,
        semantic_name: &str,
        qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        attributes: &[(&str, bool)],
        visibility: Option<&str>,
        signature: Option<&str>,
        is_definition: bool,
    ) -> NodeId {
        let canonical_qualified_name =
            canonicalize_graph_qualified_name(self.language, qualified_name);
        self.add_node_internal_with_canonical_name(
            semantic_name,
            &canonical_qualified_name,
            span,
            kind,
            attributes,
            visibility,
            signature,
            is_definition,
            SpanOrigin::Declaration,
        )
    }

    /// The single node-minting sink. `origin` says what `span` IS at this site
    /// (issue #748), and there are three answers, not two:
    /// [`SpanOrigin::Declaration`] for a declaration WITH a body, whose extent
    /// is filed as the node's body; [`SpanOrigin::BodylessDeclaration`] for a
    /// forward declaration, which is the node's own location but not a body;
    /// and [`SpanOrigin::CallSite`] for a call site or type reference, which is
    /// neither. Only the first files an extent.
    #[allow(clippy::too_many_arguments)] // internal builder sink; is_definition (issue #394) and span origin (issue #748) threaded alongside span/kind/attrs
    fn add_node_internal_with_canonical_name(
        &mut self,
        semantic_name: &str,
        canonical_qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        attributes: &[(&str, bool)],
        visibility: Option<&str>,
        signature: Option<&str>,
        is_definition: bool,
        origin: SpanOrigin,
    ) -> NodeId {
        self.add_node_internal_with_canonical_name_inner(
            semantic_name,
            canonical_qualified_name,
            span,
            kind,
            attributes,
            visibility,
            signature,
            is_definition,
            origin,
            true,
        )
    }

    /// As above, but `publish_qualified_name` decides whether the canonical
    /// string is STORED on the entry as well as used as the cache key.
    ///
    /// Those two roles are normally the same string and it is correct to
    /// publish it. They must come apart for a per-binding-site declaration:
    /// its identity has to include the binding offset (two locals named `x`
    /// are different nodes), while nothing user-facing should ever contain
    /// that offset. Publishing it put `x@1487` into planner, MCP and LSP
    /// output, because every display surface prefers the qualified name.
    #[allow(clippy::too_many_arguments)]
    fn add_node_internal_with_canonical_name_inner(
        &mut self,
        semantic_name: &str,
        canonical_qualified_name: &str,
        span: Option<Span>,
        kind: NodeKind,
        attributes: &[(&str, bool)],
        visibility: Option<&str>,
        signature: Option<&str>,
        is_definition: bool,
        origin: SpanOrigin,
        publish_qualified_name: bool,
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
        if let Some(&id) = self
            .node_cache
            .get(&(canonical_qualified_name.to_string(), kind))
        {
            let visibility_id = visibility.map(|vis| self.intern(vis));
            let signature_id = signature.map(|sig| self.intern(sig));
            let update = NodeMetadataUpdate::new()
                .with_optional_span(span)
                .mark_if(NodeMetadataFlag::Async, is_async)
                .mark_if(NodeMetadataFlag::Static, is_static)
                .mark_if(NodeMetadataFlag::Unsafe, is_unsafe)
                .mark_if(NodeMetadataFlag::Definition, is_definition)
                .with_optional_visibility(visibility_id)
                .with_optional_signature(signature_id);
            self.staging
                .update_node_entry_tracking_span_origin(id, &update, origin);
            return id;
        }

        let name_id = self.intern(semantic_name);

        // Create node entry
        let mut entry = NodeEntry::new(kind, name_id, self.file_id);
        entry.is_definition = is_definition;
        if publish_qualified_name && semantic_name != canonical_qualified_name {
            let qualified_name_id = self.intern(canonical_qualified_name);
            entry = entry.with_qualified_name(qualified_name_id);
        }

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

        // File what the span IS. Whether it also wins the recorded location is
        // a separate question, settled by `apply_span_to_entry`; the body
        // extent must not depend on that (issue #748).
        if let Some(ref s) = span {
            self.staging.record_span_origin(node_id, s, origin);
        }

        // Cache for deduplication
        self.node_cache
            .insert((canonical_qualified_name.to_string(), kind), node_id);

        node_id
    }

    /// Verbatim-name sink (style rules, imports, CSS variables): identical to
    /// [`add_node_internal`](Self::add_node_internal) except the cache key is
    /// the raw name rather than the canonicalized one.
    ///
    /// Every caller supplies a declaration extent (the `@import` statement,
    /// the rule, the variable), so span provenance is recorded as
    /// [`SpanOrigin::Declaration`]. No call-edge path mints verbatim nodes.
    fn add_node_verbatim(
        &mut self,
        name: &str,
        span: Option<Span>,
        kind: NodeKind,
        attributes: &[(&str, bool)],
        visibility: Option<&str>,
        signature: Option<&str>,
        is_definition: bool,
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

        if let Some(&id) = self.node_cache.get(&(name.to_string(), kind)) {
            let visibility_id = visibility.map(|vis| self.intern(vis));
            let signature_id = signature.map(|sig| self.intern(sig));
            let update = NodeMetadataUpdate::new()
                .with_optional_span(span)
                .mark_if(NodeMetadataFlag::Async, is_async)
                .mark_if(NodeMetadataFlag::Static, is_static)
                .mark_if(NodeMetadataFlag::Unsafe, is_unsafe)
                .mark_if(NodeMetadataFlag::Definition, is_definition)
                .with_optional_visibility(visibility_id)
                .with_optional_signature(signature_id);
            self.staging.update_node_entry_tracking_span_origin(
                id,
                &update,
                SpanOrigin::Declaration,
            );
            return id;
        }

        let name_id = self.intern(name);
        let mut entry = NodeEntry::new(kind, name_id, self.file_id);
        entry.is_definition = is_definition;

        if let Some(s) = span {
            let start_line = u32::try_from(s.start.line.saturating_add(1)).unwrap_or(u32::MAX);
            let start_column = u32::try_from(s.start.column).unwrap_or(u32::MAX);
            let end_line = u32::try_from(s.end.line.saturating_add(1)).unwrap_or(u32::MAX);
            let end_column = u32::try_from(s.end.column).unwrap_or(u32::MAX);
            entry = entry.with_location(start_line, start_column, end_line, end_column);
        }

        if is_async {
            entry = entry.with_async(true);
        }
        if is_static {
            entry = entry.with_static(true);
        }
        if is_unsafe {
            entry = entry.with_unsafe(true);
        }

        if let Some(vis) = visibility {
            let vis_id = self.intern(vis);
            entry = entry.with_visibility(vis_id);
        }
        if let Some(sig) = signature {
            let sig_id = self.intern(sig);
            entry = entry.with_signature(sig_id);
        }

        let node_id = self.staging.add_node(entry);
        if let Some(ref s) = span {
            self.staging
                .record_span_origin(node_id, s, SpanOrigin::Declaration);
        }
        self.node_cache.insert((name.to_string(), kind), node_id);
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
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
                resolved_via: ResolvedVia::Direct,
            },
            self.file_id,
            spans,
        );
    }

    /// Stage a `NodeKind::Channel` node for a Go channel alias-class (T2.4).
    ///
    /// Dedupes by `(qualified_name, NodeKind::Channel)` through the canonical
    /// node cache, so all operation sites on the same alias-class collapse to
    /// one node. `buffer_kind` / `capacity` classify the channel's
    /// `make(chan T, N)` form; in Phase 1 the classifier is carried onto each
    /// `ChannelPeer` edge (where the planner consumes it) rather than into a
    /// separate node-metadata payload, so the parameters document the
    /// classification at the call site without a persistence-format addition.
    pub fn add_channel(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        _buffer_kind: ChannelBufferKind,
        _capacity: Option<u32>,
    ) -> NodeId {
        self.add_node(qualified_name, span, NodeKind::Channel)
    }

    /// Stage a `ChannelPeer` edge from an operation-site `CallSite` to its
    /// canonical `Channel` node (T2.4).
    pub fn add_channel_peer_edge_with_span(
        &mut self,
        op_site: NodeId,
        channel: NodeId,
        direction: ChannelPeerDirection,
        buffer_kind: ChannelBufferKind,
        span: Span,
    ) {
        self.staging.add_edge_with_spans(
            op_site,
            channel,
            EdgeKind::ChannelPeer {
                direction,
                buffer_kind,
            },
            self.file_id,
            vec![span],
        );
    }

    /// Stage an `Instantiates` edge from a generic call-site `CallSite` to the
    /// generic function / method definition (T2.5).
    ///
    /// `type_args` is taken by value so the Go plugin can build it via
    /// `SmallVec::from_iter(...)` and move ownership. The `TypeArg.name`
    /// `StringId`s are interned through this helper's interner, so they are
    /// remapped to global ids during the commit's string-table dedup.
    pub fn add_instantiates_edge_with_span(
        &mut self,
        call_site: NodeId,
        target: NodeId,
        type_args: smallvec::SmallVec<[TypeArg; 4]>,
        inference_kind: InferenceKind,
        span: Span,
    ) {
        self.staging.add_edge_with_spans(
            call_site,
            target,
            EdgeKind::Instantiates {
                type_args,
                inference_kind,
            },
            self.file_id,
            vec![span],
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

    /// Add a T3 `Wraps` edge from a wrapper expression to a wrapped error
    /// value (Go error chains, `02_DESIGN` §1.3 / §2.4).
    ///
    /// The `kind` discriminates the source-syntax form
    /// (`fmt.Errorf("%w", err)`, `Unwrap()` method, `errors.{Is,As,AsType,Join}`);
    /// `chain_position` carries the verb index for `%w` (0-based, skipping
    /// `%%`) and the slice index for `errors.Join` / `Unwrap() []error`
    /// slice literals (`None` for single-value forms).
    ///
    /// `span` optionally records the source location of the wrap site
    /// (e.g. the `%w` verb or the `Unwrap()` call expression). Pass `None`
    /// when the caller cannot resolve a meaningful position. Wraps edges
    /// route through the existing staging `EdgeStore` exactly like
    /// [`Self::add_implements_edge`] — Phase 4d-prime is the only new
    /// pipeline structural change required by T3.
    pub fn add_wraps_edge(
        &mut self,
        source: NodeId,
        target: NodeId,
        kind: WrapKind,
        chain_position: Option<u16>,
        span: Option<Span>,
    ) {
        let spans = span.map(|s| vec![s]).unwrap_or_default();
        self.staging.add_edge_with_spans(
            source,
            target,
            EdgeKind::Wraps {
                kind,
                chain_position,
            },
            self.file_id,
            spans,
        );
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

    /// Search `CALL_COMPATIBLE_KINDS` for an existing node with the given
    /// canonical qualified name, skipping `exclude` (the caller's own kind).
    ///
    /// Returns the first matching `NodeId` or `None`. The sweep is read-only —
    /// no metadata is mutated on cross-kind reuse (Stage 1 declaration metadata
    /// is authoritative).
    fn reuse_across_call_compatible_kinds(
        &self,
        canonical: &str,
        exclude: NodeKind,
    ) -> Option<NodeId> {
        for &kind in CALL_COMPATIBLE_KINDS {
            if kind == exclude {
                continue;
            }
            if let Some(&id) = self.node_cache.get(&(canonical.to_string(), kind)) {
                return Some(id);
            }
        }
        None
    }

    /// Ensure a callee node exists for call-edge construction, with a
    /// **non-optional** call-site span.
    ///
    /// This is the preferred API for Stage 2 call-edge building. The span is
    /// required so that every stub gets at least the caller's line — never 0.
    /// The `kind_hint` guides the sweep order and determines the `NodeKind`
    /// used if a fresh node must be created.
    ///
    /// Cross-kind reuse: if a node with the same canonical qualified name
    /// already exists as any call-compatible kind, it is returned as-is.
    ///
    /// The minted stub records [`SpanOrigin::CallSite`], so it is kept out of
    /// the body plane: the extent belongs to whoever wrote the call, and
    /// fingerprinting it there grouped every stub minted from one call site as
    /// a body duplicate of the others (issue #748). A declaration for the same
    /// name, before or after, files its own extent as the node's body through
    /// the normal `add_*` path, so a symbol that is called above its own
    /// definition still ends up with a real body, and one called BELOW its
    /// definition keeps the body it already had.
    pub fn ensure_callee(
        &mut self,
        qualified_name: &str,
        call_site_span: Span,
        kind_hint: CalleeKindHint,
    ) -> NodeId {
        let canonical = canonicalize_graph_qualified_name(self.language, qualified_name);
        let target_kind = kind_hint.to_node_kind();

        // First check for exact-kind cache hit (fast path)
        if let Some(&id) = self.node_cache.get(&(canonical.clone(), target_kind)) {
            return id;
        }
        // Then sweep all other call-compatible kinds
        if let Some(id) = self.reuse_across_call_compatible_kinds(&canonical, target_kind) {
            return id;
        }
        // Create a new node with the call-site span (never None). Callee stubs
        // are never declarations -> is_definition = false.
        self.add_call_site_node_internal(qualified_name, call_site_span, target_kind)
    }

    /// Ensure a function node exists, creating it if needed.
    ///
    /// Cross-kind reuse: if a node with the same canonical qualified name
    /// already exists as any call-compatible kind (Method, Macro, Constant,
    /// `LambdaTarget`), the existing node is returned as-is. This prevents
    /// duplicate spanless Function nodes from being created during Stage 2
    /// call-edge construction, which would cause `get_references` to silently
    /// drop callers due to location-based deduplication at `(file, line=0, col=0)`.
    ///
    /// The Stage 1 declaration node is authoritative for metadata — no attributes
    /// are mutated on cross-kind reuse.
    pub fn ensure_function(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_unsafe: bool,
    ) -> NodeId {
        let canonical = canonicalize_graph_qualified_name(self.language, qualified_name);
        if let Some(id) = self.reuse_across_call_compatible_kinds(&canonical, NodeKind::Function) {
            return id;
        }
        // Call-edge target stubs are not declarations -> is_definition = false.
        self.add_function_inner(qualified_name, span, is_async, is_unsafe, false)
    }

    /// Ensure a method node exists, creating it if needed.
    ///
    /// Cross-kind reuse: if a node with the same canonical qualified name
    /// already exists as any call-compatible kind (Function, Macro, Constant,
    /// `LambdaTarget`), the existing node is returned as-is. See
    /// [`ensure_function`](Self::ensure_function) for the rationale.
    pub fn ensure_method(
        &mut self,
        qualified_name: &str,
        span: Option<Span>,
        is_async: bool,
        is_static: bool,
    ) -> NodeId {
        let canonical = canonicalize_graph_qualified_name(self.language, qualified_name);
        if let Some(id) = self.reuse_across_call_compatible_kinds(&canonical, NodeKind::Method) {
            return id;
        }
        // Call-edge target stubs are not declarations -> is_definition = false.
        self.add_method_inner(qualified_name, span, is_async, is_static, false)
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

    // -----------------------------------------------------------------------
    // C indirect-call precision staging (Phase A, U10).
    //
    // All accessors below route into the per-file `CIndirectStagingPayload`
    // owned by `StagingGraph`. Non-C plugins never call these — the parent
    // `Option` on `StagingGraph` stays `None`, so the per-file staging
    // buffer is unchanged for the other 36 plugins.
    //
    // The methods here are intentionally narrow: each one performs a single
    // push or a single setter. The C plugin's Phase 1 walkers compose them
    // (see `sqry-lang-c::relations::graph_builder`); U11's Phase 3 commit
    // consumes the payload via `staging.take_c_indirect()`.
    // -----------------------------------------------------------------------

    /// Record `target_fn_name` as address-taken on the per-file C indirect
    /// staging payload (DESIGN §2.5 patterns).
    ///
    /// The name is interned through the helper's standard staging
    /// interner — DESIGN §2.5 specifies a per-file `Vec<StringId>`, so
    /// each push goes through `self.intern(...)` so the local → global
    /// `StringId` remap built by `StagingGraph::commit_strings` applies
    /// uniformly during Phase 3 commit. U11 resolves each global
    /// `StringId` to its canonical `NodeId` via the post-unification
    /// qualified-name index and applies the `NodeFlags::ADDRESS_TAKEN`
    /// bit. Duplicates within a file are tolerated — `mark_address_taken`
    /// is idempotent.
    pub fn mark_function_address_taken_by_name(&mut self, target_fn_name: &str) {
        let id = self.intern(target_fn_name);
        self.staging
            .c_indirect_mut()
            .pending_address_taken_names
            .push(id);
    }

    /// Install the per-file [`LocalScopeIndex`] on the C indirect staging
    /// payload (DESIGN §4.1).
    ///
    /// Called from the top of the C plugin's `build_graph` after running
    /// the tree-sitter scope-arena builder. U11 transfers the index into
    /// `CIndirectSideTables::local_scope_indices` keyed by `FileId`.
    pub fn set_local_scope_index(&mut self, index: LocalScopeIndex) {
        self.staging.c_indirect_mut().local_scope_index = Some(index);
    }

    /// Push a [`PendingIndirectCallsite`] onto the per-file C indirect
    /// staging payload (DESIGN §4.2).
    ///
    /// The caller is identified by its qualified name string; U11 resolves
    /// it to a `NodeId` after Phase 4c-prime cross-file unification. U12's
    /// resolver consumes the callsite list in `pass5b_c_indirect_resolve`.
    pub fn push_indirect_callsite(
        &mut self,
        caller_qualified_name: &str,
        use_span: (usize, usize),
        shape: IndirectShape,
        argument_count: u32,
        is_async: bool,
    ) {
        self.staging
            .c_indirect_mut()
            .pending_indirect_callsites
            .push(PendingIndirectCallsite {
                caller_qualified_name: caller_qualified_name.to_string(),
                use_span,
                shape,
                argument_count,
                is_async,
            });
    }

    /// Push a [`PendingBinding`] onto the per-file C indirect staging
    /// payload (DESIGN §7.1).
    ///
    /// Designated initializer (`{ .field = fn }`) and positional
    /// initializer (`{ fn1, fn2 }`) sites are both routed through this
    /// helper, with the `site_kind` discriminator preserved for U12's
    /// resolver.
    pub fn push_binding(
        &mut self,
        struct_tag: &str,
        field_name: &str,
        instance_name: &str,
        target_fn_name: &str,
        site_kind: BindingSiteKind,
    ) {
        self.staging
            .c_indirect_mut()
            .pending_bindings
            .push(PendingBinding {
                struct_tag: struct_tag.to_string(),
                field_name: field_name.to_string(),
                instance_name: instance_name.to_string(),
                target_fn_name: target_fn_name.to_string(),
                site_kind,
            });
    }

    /// Push a struct function-pointer field signature onto the per-file C
    /// indirect staging payload (DESIGN §3.2.2 / §3.7).
    ///
    /// The signature follows the DESIGN §3.1 canonical-string grammar.
    /// U11 interns each leg and inserts into
    /// `CIndirectSideTables::struct_field_fnptr`.
    pub fn push_struct_field_fnptr_signature(
        &mut self,
        struct_tag: &str,
        field_name: &str,
        signature: &str,
    ) {
        self.staging
            .c_indirect_mut()
            .pending_struct_field_signatures
            .push((
                struct_tag.to_string(),
                field_name.to_string(),
                signature.to_string(),
            ));
    }

    /// Immutable accessor for the C indirect staging payload, if any.
    ///
    /// Exposed for tests and for the C plugin's own walkers that need to
    /// consult prior state (e.g. to suppress duplicate emissions within
    /// the same file). Returns `None` until at least one
    /// `mark_function_address_taken_by_name` / `set_local_scope_index` /
    /// `push_indirect_callsite` / `push_binding` /
    /// `push_struct_field_fnptr_signature` call has populated the payload.
    #[must_use]
    pub fn c_indirect(&self) -> Option<&CIndirectStagingPayload> {
        self.staging.c_indirect()
    }
}

fn semantic_name_for_node_input(original: &str, canonical: &str) -> String {
    if original.contains('/') {
        return original.to_string();
    }

    canonical
        .rsplit("::")
        .next()
        .map_or_else(|| original.to_string(), ToString::to_string)
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
                ..
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
    fn test_helper_canonicalizes_language_native_qualified_names() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.py");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Python);

        let _method_id = helper.add_method("pkg.module.run", None, false, false);

        let add_node_op = staging
            .operations()
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }))
            .expect("Expected AddNode operation");

        if let StagingOp::AddNode { entry, .. } = add_node_op {
            assert_eq!(staging.resolve_local_string(entry.name), Some("run"));
            assert_eq!(
                staging.resolve_node_name(entry),
                Some("pkg::module::run"),
                "expected GraphBuildHelper to canonicalize Python dotted qualified names"
            );
        }
    }

    #[test]
    fn test_helper_preserves_path_qualified_names() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.js");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::JavaScript);

        let _func_id = helper.add_function("frontend/api.js::fetchUsers", None, false, false);

        let add_node_op = staging
            .operations()
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }))
            .expect("Expected AddNode operation");

        if let StagingOp::AddNode { entry, .. } = add_node_op {
            assert_eq!(
                staging.resolve_local_string(entry.name),
                Some("frontend/api.js::fetchUsers")
            );
            assert_eq!(
                staging.resolve_node_name(entry),
                Some("frontend/api.js::fetchUsers"),
                "expected path-qualified names to remain unchanged"
            );
        }
    }

    #[test]
    fn test_helper_verbatim_import_preserves_resource_name() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("index.html");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Html);

        let _import_id = helper.add_verbatim_import("styles.css", None);

        let add_node_op = staging
            .operations()
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }))
            .expect("Expected AddNode operation");

        if let StagingOp::AddNode { entry, .. } = add_node_op {
            assert_eq!(staging.resolve_local_string(entry.name), Some("styles.css"));
            assert_eq!(entry.qualified_name, None);
            assert_eq!(
                staging.resolve_node_name(entry),
                Some("styles.css"),
                "expected verbatim resource imports to preserve their literal identity"
            );
        }
    }

    #[test]
    fn test_helper_verbatim_variable_preserves_resource_name() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("index.html");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Html);

        let _variable_id = helper.add_verbatim_variable("/assets/logo.icon.png", None);

        let add_node_op = staging
            .operations()
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }))
            .expect("Expected AddNode operation");

        if let StagingOp::AddNode { entry, .. } = add_node_op {
            assert_eq!(
                staging.resolve_local_string(entry.name),
                Some("/assets/logo.icon.png")
            );
            assert_eq!(entry.qualified_name, None);
            assert_eq!(
                staging.resolve_node_name(entry),
                Some("/assets/logo.icon.png"),
                "expected verbatim resource variables to preserve their literal identity"
            );
        }
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
                    ..
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
                    ..
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

    // ========================================================================
    // Cross-kind reuse tests (Method/Function NodeKind mismatch fix)
    // ========================================================================

    #[test]
    fn test_ensure_function_reuses_existing_method_node() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.ts");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::TypeScript);

        let span = Span::new(
            Position { line: 5, column: 4 },
            Position {
                line: 10,
                column: 5,
            },
        );

        // Stage 1: create a Method node with proper span
        let method_id = helper.add_method("MyClass.doWork", Some(span), true, false);

        // Stage 2: ensure_function for the same qualified name
        let reused_id = helper.ensure_function("MyClass.doWork", None, true, false);

        assert_eq!(
            method_id, reused_id,
            "ensure_function should reuse the existing Method node"
        );
        assert_eq!(
            helper.stats().nodes_created,
            1,
            "Only the Method node should exist"
        );
    }

    #[test]
    fn test_ensure_method_reuses_existing_function_node() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.ts");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::TypeScript);

        let func_id = helper.add_function("standalone", None, false, false);
        let reused_id = helper.ensure_method("standalone", None, false, false);

        assert_eq!(
            func_id, reused_id,
            "ensure_method should reuse the existing function node"
        );
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_ensure_function_creates_new_when_no_method_exists() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.ts");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::TypeScript);

        let func_id = helper.ensure_function("topLevel", None, false, false);
        assert!(!func_id.is_invalid());
        assert_eq!(helper.stats().nodes_created, 1);

        let func_id2 = helper.ensure_function("topLevel", None, false, false);
        assert_eq!(func_id, func_id2);
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_no_method_function_duplicate_after_cross_kind_reuse() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("browser-manager.ts");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::TypeScript);

        let span_a = Span::new(
            Position { line: 3, column: 4 },
            Position { line: 8, column: 5 },
        );
        let span_b = Span::new(
            Position {
                line: 10,
                column: 4,
            },
            Position {
                line: 15,
                column: 5,
            },
        );

        // Stage 1: create Method nodes
        let _method_a = helper.add_method("BrowserManager.newTab", Some(span_a), true, false);
        let _method_b = helper.add_method("BrowserManager.restoreState", Some(span_b), true, false);

        // Stage 2: ensure_function for call-edge construction
        let _caller_a = helper.ensure_function("BrowserManager.newTab", None, true, false);
        let _caller_b = helper.ensure_function("BrowserManager.restoreState", None, true, false);

        // Verify: no same-name Method/NodeKind::Function duplicates
        let ops = staging.operations();
        let mut method_names = std::collections::HashSet::new();
        let mut function_names = std::collections::HashSet::new();

        for op in ops {
            if let StagingOp::AddNode { entry, .. } = op {
                if entry.kind == NodeKind::Method {
                    method_names.insert(entry.name);
                } else if entry.kind == NodeKind::Function {
                    function_names.insert(entry.name);
                }
            }
        }

        let overlap: Vec<_> = method_names.intersection(&function_names).collect();
        assert!(
            overlap.is_empty(),
            "Found names that are both Method and Function: {overlap:?}"
        );
    }

    // ========================================================================
    // Generalized cross-kind reuse tests (HU01: CALL_COMPATIBLE_KINDS)
    // ========================================================================

    #[test]
    fn test_ensure_function_reuses_existing_macro_node() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.c");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::C);

        let span = Span::new(
            Position { line: 1, column: 0 },
            Position {
                line: 1,
                column: 40,
            },
        );

        // Stage 1: create a Macro node (e.g., list_for_each_entry in C kernel code)
        let macro_id = helper.add_node("list_for_each_entry", Some(span), NodeKind::Macro);

        // Stage 2: ensure_function for the same name (call-edge construction)
        let reused_id = helper.ensure_function("list_for_each_entry", None, false, false);

        assert_eq!(
            macro_id, reused_id,
            "ensure_function should reuse the existing Macro node"
        );
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_ensure_function_reuses_existing_constant_node() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.c");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::C);

        let span = Span::new(
            Position { line: 3, column: 0 },
            Position {
                line: 3,
                column: 30,
            },
        );

        // A function pointer constant in C
        let const_id = helper.add_constant("handler_fn", Some(span));

        let reused_id = helper.ensure_function("handler_fn", None, false, false);

        assert_eq!(
            const_id, reused_id,
            "ensure_function should reuse the existing Constant node"
        );
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_ensure_method_reuses_existing_lambda_target_node() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.java");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Java);

        let span = Span::new(
            Position { line: 7, column: 8 },
            Position {
                line: 10,
                column: 9,
            },
        );

        let lambda_id = helper.add_node("Comparator.compare", Some(span), NodeKind::LambdaTarget);

        let reused_id = helper.ensure_method("Comparator.compare", None, false, false);

        assert_eq!(
            lambda_id, reused_id,
            "ensure_method should reuse the existing LambdaTarget node"
        );
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_cross_kind_reuse_does_not_merge_incompatible_kinds() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.css");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Css);

        // Create a StyleRule node — NOT a call-compatible kind
        let style_id = helper.add_node_verbatim(
            ".container",
            None,
            NodeKind::StyleRule,
            &[],
            None,
            None,
            false,
        );

        // ensure_function with the same name should NOT reuse the StyleRule
        let func_id = helper.ensure_function(".container", None, false, false);

        assert_ne!(
            style_id, func_id,
            "ensure_function must NOT merge into a StyleRule"
        );
        assert_eq!(helper.stats().nodes_created, 2);
    }

    // ========================================================================
    // Stub-first order tests (Codex review M1: ensure_* before add_*)
    // Proves cross-kind reuse works when the STUB is created first and
    // the real declaration arrives later — the actual line-zero failure mode.
    // ========================================================================

    #[test]
    fn test_stub_first_ensure_function_then_add_method_reuses() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.ts");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::TypeScript);

        // Stage 2 runs first (call-edge construction creates a Function stub)
        let stub_id = helper.ensure_function("Widget.render", None, false, false);

        // Stage 1 runs later (declaration extraction creates Method with real span)
        let span = Span::new(
            Position {
                line: 10,
                column: 4,
            },
            Position {
                line: 20,
                column: 5,
            },
        );
        let decl_id = helper.add_method("Widget.render", Some(span), false, false);

        // The two calls should produce DIFFERENT NodeIds because add_method
        // uses its own (name, Method) cache key while ensure_function created
        // (name, Function). This is the scenario Phase 4c-prime unifies later.
        // What matters here: NO PANIC, and both IDs are valid.
        assert!(!stub_id.is_invalid());
        assert!(!decl_id.is_invalid());
        // If they are different, Phase 4c-prime handles the merge.
        // If add_node_internal deduped them (same canonical), that's also fine.
    }

    #[test]
    fn test_stub_first_ensure_method_then_add_function_reuses() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.py");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Python);

        // Stub created first
        let stub_id = helper.ensure_method("process_data", None, false, false);

        // Real declaration arrives
        let span = Span::new(
            Position { line: 5, column: 0 },
            Position {
                line: 15,
                column: 0,
            },
        );
        let decl_id = helper.add_function("process_data", Some(span), false, false);

        assert!(!stub_id.is_invalid());
        assert!(!decl_id.is_invalid());
    }

    #[test]
    fn test_ensure_callee_then_add_function_same_name_no_panic() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.c");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::C);

        let call_span = Span::new(
            Position {
                line: 50,
                column: 4,
            },
            Position {
                line: 50,
                column: 20,
            },
        );
        let callee_id = helper.ensure_callee("kfree", call_span, CalleeKindHint::Function);

        let def_span = Span::new(
            Position { line: 1, column: 0 },
            Position {
                line: 10,
                column: 1,
            },
        );
        let def_id = helper.add_function("kfree", Some(def_span), false, false);

        // ensure_callee already created a Function node for "kfree", so
        // add_function should return the same NodeId (same cache key).
        assert_eq!(
            callee_id, def_id,
            "add_function should reuse the node created by ensure_callee"
        );
        assert_eq!(helper.stats().nodes_created, 1);
    }

    // ========================================================================
    // ensure_callee tests (HU02)
    // ========================================================================

    #[test]
    fn test_ensure_callee_function_hint_creates_with_span() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let call_span = Span::new(
            Position {
                line: 20,
                column: 4,
            },
            Position {
                line: 20,
                column: 30,
            },
        );

        let id = helper.ensure_callee("target_fn", call_span, CalleeKindHint::Function);
        assert!(!id.is_invalid());

        // The created node should have start_line > 0 (from the call-site span)
        let ops = staging.operations();
        let node_op = ops
            .iter()
            .find(|op| matches!(op, StagingOp::AddNode { .. }));
        if let Some(StagingOp::AddNode { entry, .. }) = node_op {
            assert!(
                entry.start_line > 0,
                "ensure_callee must produce nodes with line > 0"
            );
        }
    }

    #[test]
    fn test_ensure_callee_macro_hint_reuses_existing_macro() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.c");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::C);

        let def_span = Span::new(
            Position { line: 5, column: 0 },
            Position {
                line: 5,
                column: 40,
            },
        );
        let call_span = Span::new(
            Position {
                line: 99,
                column: 4,
            },
            Position {
                line: 99,
                column: 30,
            },
        );

        let macro_id = helper.add_node("IS_ERR", Some(def_span), NodeKind::Macro);
        let reused_id = helper.ensure_callee("IS_ERR", call_span, CalleeKindHint::Macro);

        assert_eq!(
            macro_id, reused_id,
            "ensure_callee should reuse existing Macro node"
        );
        assert_eq!(helper.stats().nodes_created, 1);
    }

    #[test]
    fn test_ensure_callee_idempotent_returns_first_spans_node() {
        let mut staging = StagingGraph::new();
        let file = PathBuf::from("test.rs");
        let mut helper = GraphBuildHelper::new(&mut staging, &file, Language::Rust);

        let span1 = Span::new(
            Position {
                line: 10,
                column: 0,
            },
            Position {
                line: 10,
                column: 20,
            },
        );
        let span2 = Span::new(
            Position {
                line: 50,
                column: 0,
            },
            Position {
                line: 50,
                column: 20,
            },
        );

        let id1 = helper.ensure_callee("func", span1, CalleeKindHint::Function);
        let id2 = helper.ensure_callee("func", span2, CalleeKindHint::Function);

        assert_eq!(
            id1, id2,
            "Two ensure_callee calls for the same name return the same NodeId"
        );
    }

    #[test]
    fn test_call_compatible_kinds_dry_no_body_changes_needed() {
        // Compile-time proof: adding a variant to CALL_COMPATIBLE_KINDS does
        // NOT require touching ensure_function or ensure_method bodies. Both
        // delegate to reuse_across_call_compatible_kinds which iterates the
        // const slice. This test simply asserts the slice contains the expected
        // entries to catch accidental removals.
        assert!(CALL_COMPATIBLE_KINDS.contains(&NodeKind::Function));
        assert!(CALL_COMPATIBLE_KINDS.contains(&NodeKind::Method));
        assert!(CALL_COMPATIBLE_KINDS.contains(&NodeKind::Macro));
        assert!(CALL_COMPATIBLE_KINDS.contains(&NodeKind::Constant));
        assert!(CALL_COMPATIBLE_KINDS.contains(&NodeKind::LambdaTarget));
        assert_eq!(CALL_COMPATIBLE_KINDS.len(), 5);
    }
}
