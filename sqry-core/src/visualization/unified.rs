//! Unified graph visualization exporters.
//!
//! This module provides visualization exporters that work directly with
//! [`GraphSnapshot`](crate::graph::unified::concurrent::GraphSnapshot) from the unified graph architecture, replacing the
//! legacy exporters that operated on the pre-unified graph.
//!
//! #
//!
//! These exporters are the migration target for the legacy visualization modules.
//! They provide the same functionality but use the unified graph's efficient
//! Arena+CSR storage instead of DashMap iteration.
//!
//! # Available Exporters
//!
//! - [`UnifiedDotExporter`](crate::visualization::unified::UnifiedDotExporter): Graphviz DOT format
//! - [`UnifiedD2Exporter`](crate::visualization::unified::UnifiedD2Exporter): D2 diagram format
//! - [`UnifiedJsonExporter`](crate::visualization::unified::UnifiedJsonExporter): JSON format for web visualizations
//! - [`UnifiedMermaidExporter`](crate::visualization::unified::UnifiedMermaidExporter): Mermaid format for Markdown
//!
//! # Example
//!
//! ```rust,ignore
//! use sqry_core::graph::unified::concurrent::GraphSnapshot;
//! use sqry_core::visualization::unified::{UnifiedDotExporter, DotConfig};
//!
//! let snapshot: GraphSnapshot = /* ... */;
//! let config = DotConfig::default()
//!     .with_cross_language_highlight(true)
//!     .with_details(true);
//! let exporter = UnifiedDotExporter::new(&snapshot, config);
//! let dot_output = exporter.export();
//! ```

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write;

use crate::graph::node::Language;
use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::node::kind::NodeKind;
use crate::graph::unified::storage::arena::NodeEntry;

// ============================================================================
// Common Types
// ============================================================================

/// Graph layout direction for visualization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// Left to right (default)
    #[default]
    LeftToRight,
    /// Top to bottom
    TopToBottom,
}

impl Direction {
    /// Returns the direction as a string for DOT/D2.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LeftToRight => "LR",
            Self::TopToBottom => "TB",
        }
    }
}

/// Edge kind filter for filtering edges by type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeFilter {
    /// Call edges
    Calls,
    /// Import edges
    Imports,
    /// Export edges
    Exports,
    /// Reference edges
    References,
    /// Inheritance edges
    Inherits,
    /// Implementation edges
    Implements,
    /// FFI call edges
    FfiCall,
    /// HTTP request edges
    HttpRequest,
    /// Database query edges
    DbQuery,
}

impl EdgeFilter {
    /// Check if an `EdgeKind` matches this filter.
    #[must_use]
    pub fn matches(&self, kind: &EdgeKind) -> bool {
        matches!(
            (self, kind),
            (EdgeFilter::Calls, EdgeKind::Calls { .. })
                | (EdgeFilter::Imports, EdgeKind::Imports { .. })
                | (EdgeFilter::Exports, EdgeKind::Exports { .. })
                | (EdgeFilter::References, EdgeKind::References)
                | (EdgeFilter::Inherits, EdgeKind::Inherits)
                | (EdgeFilter::Implements, EdgeKind::Implements)
                | (EdgeFilter::FfiCall, EdgeKind::FfiCall { .. })
                | (EdgeFilter::HttpRequest, EdgeKind::HttpRequest { .. })
                | (EdgeFilter::DbQuery, EdgeKind::DbQuery { .. })
        )
    }
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Get color for a language.
#[must_use]
pub fn language_color(lang: Language) -> &'static str {
    match lang {
        Language::Rust => "#dea584",
        Language::JavaScript => "#f7df1e",
        Language::TypeScript => "#3178c6",
        Language::Python => "#3572A5",
        Language::Go => "#00ADD8",
        Language::Java => "#b07219",
        Language::Ruby => "#701516",
        Language::Php => "#4F5D95",
        Language::Cpp => "#f34b7d",
        Language::C => "#555555",
        Language::Swift => "#F05138",
        Language::Kotlin => "#A97BFF",
        Language::Scala => "#c22d40",
        Language::Sql | Language::Plsql => "#e38c00",
        Language::Shell => "#89e051",
        Language::Lua => "#000080",
        Language::Perl => "#0298c3",
        Language::Dart => "#00B4AB",
        Language::Groovy => "#4298b8",
        Language::Http => "#005C9C",
        Language::Css => "#563d7c",
        Language::Elixir => "#6e4a7e",
        Language::R => "#198CE7",
        Language::Haskell => "#5e5086",
        Language::Html => "#e34c26",
        Language::Svelte => "#ff3e00",
        Language::Vue => "#41b883",
        Language::Zig => "#ec915c",
        Language::Terraform => "#5c4ee5",
        Language::Puppet => "#302B6D",
        Language::Pulumi => "#6d2df5",
        Language::Apex => "#1797c0",
        Language::Abap => "#E8274B",
        Language::ServiceNow => "#62d84e",
        Language::CSharp => "#178600",
        Language::Json => "#292929",
    }
}

/// Get default color for unknown language.
#[must_use]
pub const fn default_language_color() -> &'static str {
    "#cccccc"
}

/// Get shape for a node kind.
#[must_use]
pub fn node_shape(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Class | NodeKind::Struct => "component",
        NodeKind::Interface | NodeKind::Trait => "ellipse",
        NodeKind::Module => "folder",
        NodeKind::Variable | NodeKind::Constant => "note",
        NodeKind::Enum | NodeKind::EnumVariant => "hexagon",
        NodeKind::Type => "diamond",
        NodeKind::Macro => "parallelogram",
        _ => "box",
    }
}

/// Get edge style based on edge kind.
#[must_use]
pub fn edge_style(kind: &EdgeKind) -> (&'static str, &'static str) {
    match kind {
        EdgeKind::Calls { .. } => ("solid", "#333333"),
        EdgeKind::Imports { .. } => ("dashed", "#0066cc"),
        EdgeKind::Exports { .. } => ("dashed", "#00cc66"),
        EdgeKind::References => ("dotted", "#666666"),
        EdgeKind::Inherits => ("solid", "#990099"),
        EdgeKind::Implements => ("dashed", "#990099"),
        EdgeKind::FfiCall { .. } => ("bold", "#ff6600"),
        EdgeKind::HttpRequest { .. } => ("bold", "#cc0000"),
        EdgeKind::DbQuery { .. } => ("bold", "#009900"),
        _ => ("solid", "#666666"),
    }
}

/// Get label for an edge kind.
#[must_use]
pub fn edge_label(
    kind: &EdgeKind,
    strings: &crate::graph::unified::storage::interner::StringInterner,
) -> String {
    match kind {
        EdgeKind::Calls {
            argument_count,
            is_async,
        } => {
            if *is_async {
                format!("async call({argument_count})")
            } else {
                format!("call({argument_count})")
            }
        }
        EdgeKind::Imports { alias, is_wildcard } => {
            if *is_wildcard {
                "import *".to_string()
            } else if let Some(alias_id) = alias {
                let alias_str = strings
                    .resolve(*alias_id)
                    .map_or_else(|| "?".to_string(), |s| s.to_string());
                format!("import as {alias_str}")
            } else {
                "import".to_string()
            }
        }
        EdgeKind::Exports {
            kind: export_kind,
            alias,
        } => {
            let kind_str = match export_kind {
                crate::graph::unified::edge::ExportKind::Direct => "export",
                crate::graph::unified::edge::ExportKind::Reexport => "re-export",
                crate::graph::unified::edge::ExportKind::Default => "default export",
                crate::graph::unified::edge::ExportKind::Namespace => "export *",
            };
            if let Some(alias_id) = alias {
                let alias_str = strings
                    .resolve(*alias_id)
                    .map_or_else(|| "?".to_string(), |s| s.to_string());
                format!("{kind_str} as {alias_str}")
            } else {
                kind_str.to_string()
            }
        }
        EdgeKind::References => "ref".to_string(),
        EdgeKind::Inherits => "extends".to_string(),
        EdgeKind::Implements => "implements".to_string(),
        EdgeKind::FfiCall { convention } => format!("ffi:{convention:?}"),
        EdgeKind::HttpRequest { method, .. } => method.as_str().to_string(),
        EdgeKind::DbQuery { query_type, .. } => format!("{query_type:?}"),
        _ => String::new(),
    }
}

/// Escape string for DOT format.
#[must_use]
pub fn escape_dot(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Escape string for D2 format.
#[must_use]
pub fn escape_d2(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ")
}

// ============================================================================
// DOT Exporter
// ============================================================================

/// Configuration for DOT export.
#[derive(Debug, Clone)]
pub struct DotConfig {
    /// Filter to specific languages (empty = all).
    pub filter_languages: HashSet<Language>,
    /// Filter to specific edge kinds (empty = all).
    pub filter_edges: HashSet<EdgeFilter>,
    /// Filter to specific files (empty = all).
    pub filter_files: HashSet<String>,
    /// Restrict rendering to a specific set of node IDs. When `Some`, only these
    /// nodes (and edges between them) are included in the output. This is a
    /// fast-path that skips BFS entirely.
    pub filter_node_ids: Option<HashSet<NodeId>>,
    /// Highlight cross-language edges.
    pub highlight_cross_language: bool,
    /// Maximum depth from root nodes (None = unlimited).
    pub max_depth: Option<usize>,
    /// Root nodes to start from (empty = all nodes).
    pub root_nodes: HashSet<NodeId>,
    /// Graph direction.
    pub direction: Direction,
    /// Show node details (file, line numbers).
    pub show_details: bool,
    /// Show edge labels.
    pub show_edge_labels: bool,
}

impl Default for DotConfig {
    fn default() -> Self {
        Self {
            filter_languages: HashSet::new(),
            filter_edges: HashSet::new(),
            filter_files: HashSet::new(),
            filter_node_ids: None,
            highlight_cross_language: false,
            max_depth: None,
            root_nodes: HashSet::new(),
            direction: Direction::LeftToRight,
            show_details: true,
            show_edge_labels: true,
        }
    }
}

impl DotConfig {
    /// Enable cross-language highlighting.
    #[must_use]
    pub fn with_cross_language_highlight(mut self, enabled: bool) -> Self {
        self.highlight_cross_language = enabled;
        self
    }

    /// Show node details.
    #[must_use]
    pub fn with_details(mut self, enabled: bool) -> Self {
        self.show_details = enabled;
        self
    }

    /// Show edge labels.
    #[must_use]
    pub fn with_edge_labels(mut self, enabled: bool) -> Self {
        self.show_edge_labels = enabled;
        self
    }

    /// Set graph direction.
    #[must_use]
    pub fn with_direction(mut self, direction: Direction) -> Self {
        self.direction = direction;
        self
    }

    /// Filter to a specific language.
    #[must_use]
    pub fn filter_language(mut self, lang: Language) -> Self {
        self.filter_languages.insert(lang);
        self
    }

    /// Filter to a specific edge kind.
    #[must_use]
    pub fn filter_edge(mut self, edge: EdgeFilter) -> Self {
        self.filter_edges.insert(edge);
        self
    }

    /// Set maximum depth.
    #[must_use]
    pub fn with_max_depth(mut self, depth: usize) -> Self {
        self.max_depth = Some(depth);
        self
    }

    /// Restrict output to the given set of node IDs. Pass `None` to clear any filter.
    /// When set, this takes priority over `root_nodes`/`max_depth` BFS traversal.
    #[must_use]
    pub fn with_filter_node_ids(mut self, ids: Option<HashSet<NodeId>>) -> Self {
        self.filter_node_ids = ids;
        self
    }
}

/// DOT format exporter for unified graph.
pub struct UnifiedDotExporter<'a> {
    graph: &'a GraphSnapshot,
    config: DotConfig,
}

impl<'a> UnifiedDotExporter<'a> {
    /// Create a new exporter with default configuration.
    #[must_use]
    pub fn new(graph: &'a GraphSnapshot) -> Self {
        Self {
            graph,
            config: DotConfig::default(),
        }
    }

    /// Create a new exporter with custom configuration.
    #[must_use]
    pub fn with_config(graph: &'a GraphSnapshot, config: DotConfig) -> Self {
        Self { graph, config }
    }

    /// Export graph to DOT format.
    #[must_use]
    pub fn export(&self) -> String {
        let mut dot = String::from("digraph CodeGraph {\n");

        // Graph attributes
        let rankdir = self.config.direction.as_str();
        writeln!(dot, "  rankdir={rankdir};").expect("write to String never fails");
        dot.push_str("  node [shape=box, style=filled];\n");
        dot.push_str("  overlap=false;\n");
        dot.push_str("  splines=true;\n\n");

        // Collect visible nodes
        let visible_nodes = self.filter_nodes();

        // Export nodes
        for node_id in &visible_nodes {
            if let Some(entry) = self.graph.get_node(*node_id) {
                self.export_node(&mut dot, *node_id, entry);
            }
        }

        dot.push('\n');

        // Export edges
        for (from, to, kind) in self.graph.iter_edges() {
            if !visible_nodes.contains(&from) || !visible_nodes.contains(&to) {
                continue;
            }

            if !self.edge_allowed(&kind) {
                continue;
            }

            self.export_edge(&mut dot, from, to, &kind);
        }

        dot.push_str("}\n");
        dot
    }

    /// Filter nodes based on configuration.
    fn filter_nodes(&self) -> HashSet<NodeId> {
        // Fastest path: explicit node ID filter from pre-computed BFS
        if let Some(ref filter_ids) = self.config.filter_node_ids {
            return filter_ids.clone();
        }

        // Fast path: no filtering
        if self.config.root_nodes.is_empty() && self.config.max_depth.is_none() {
            return self
                .graph
                .iter_nodes()
                .filter(|(id, entry)| self.should_include_node(*id, entry))
                .map(|(id, _)| id)
                .collect();
        }

        // BFS from root nodes with depth limit
        let adjacency = self.build_adjacency();
        let depth_limit = self.config.max_depth.unwrap_or(usize::MAX);
        let mut visible = HashSet::new();

        let starting_nodes: Vec<NodeId> = if self.config.root_nodes.is_empty() {
            // Gate 0d iter-2 fix: skip unified losers from the
            // visualization starting set. See
            // `NodeEntry::is_unified_loser`.
            self.graph
                .iter_nodes()
                .filter(|(_, entry)| !entry.is_unified_loser())
                .map(|(id, _)| id)
                .collect()
        } else {
            self.config.root_nodes.iter().copied().collect()
        };

        for node_id in starting_nodes {
            if visible.contains(&node_id) {
                continue;
            }
            self.collect_nodes(&mut visible, &adjacency, node_id, depth_limit);
        }

        visible
    }

    /// Check if a node should be included.
    fn should_include_node(&self, _node_id: NodeId, entry: &NodeEntry) -> bool {
        // Gate 0d iter-2 fix: never include unified losers in
        // visualization output. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            return false;
        }
        // Language filter
        if !self.config.filter_languages.is_empty() {
            if let Some(lang) = self.graph.files().language_for_file(entry.file) {
                if !self.config.filter_languages.contains(&lang) {
                    return false;
                }
            } else {
                return false;
            }
        }

        // File filter
        if !self.config.filter_files.is_empty() {
            if let Some(path) = self.graph.files().resolve(entry.file) {
                let path_str = path.to_string_lossy();
                if !self
                    .config
                    .filter_files
                    .iter()
                    .any(|f| path_str.contains(f))
                {
                    return false;
                }
            } else {
                return false;
            }
        }

        true
    }

    /// Check if an edge kind is allowed.
    fn edge_allowed(&self, kind: &EdgeKind) -> bool {
        if self.config.filter_edges.is_empty() {
            return true;
        }
        self.config.filter_edges.iter().any(|f| f.matches(kind))
    }

    /// Build adjacency map for BFS.
    fn build_adjacency(&self) -> HashMap<NodeId, Vec<NodeId>> {
        let mut adjacency: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
        for (from, to, _) in self.graph.iter_edges() {
            adjacency.entry(from).or_default().push(to);
            adjacency.entry(to).or_default().push(from);
        }
        adjacency
    }

    /// Collect nodes reachable within depth limit.
    fn collect_nodes(
        &self,
        visible: &mut HashSet<NodeId>,
        adjacency: &HashMap<NodeId, Vec<NodeId>>,
        start: NodeId,
        depth_limit: usize,
    ) {
        let mut queue = VecDeque::new();
        queue.push_back((start, 0usize));

        while let Some((node_id, depth)) = queue.pop_front() {
            if depth > depth_limit {
                continue;
            }

            let Some(entry) = self.graph.get_node(node_id) else {
                continue;
            };

            if !self.should_include_node(node_id, entry) {
                continue;
            }

            if !visible.insert(node_id) {
                continue;
            }

            if depth == depth_limit {
                continue;
            }

            if let Some(neighbors) = adjacency.get(&node_id) {
                for neighbor in neighbors {
                    queue.push_back((*neighbor, depth + 1));
                }
            }
        }
    }

    /// Export a single node to DOT.
    fn export_node(&self, dot: &mut String, node_id: NodeId, entry: &NodeEntry) {
        let lang = self.graph.files().language_for_file(entry.file);
        let color = lang.map_or(default_language_color(), language_color);
        let shape = node_shape(&entry.kind);

        // Get name - resolve to Arc<str> then use as_ref for string operations
        let name = self
            .graph
            .strings()
            .resolve(entry.name)
            .unwrap_or_else(|| std::sync::Arc::from("?"));
        let qualified_name = entry
            .qualified_name
            .and_then(|id| self.graph.strings().resolve(id))
            .unwrap_or_else(|| std::sync::Arc::clone(&name));

        // Build label
        let label = if self.config.show_details {
            let file = self
                .graph
                .files()
                .resolve(entry.file)
                .map_or_else(|| "?".to_string(), |p| p.to_string_lossy().into_owned());
            format!("{}\\n{}:{}", qualified_name, file, entry.start_line)
        } else {
            qualified_name.to_string()
        };

        let label = escape_dot(&label);
        let node_key = format!("n{}", node_id.index());

        writeln!(
            dot,
            "  \"{node_key}\" [label=\"{label}\", fillcolor=\"{color}\", shape=\"{shape}\"];",
        )
        .expect("write to String never fails");
    }

    /// Export a single edge to DOT.
    fn export_edge(&self, dot: &mut String, from: NodeId, to: NodeId, kind: &EdgeKind) {
        let (style, base_color) = edge_style(kind);
        let color = if self.config.highlight_cross_language
            && let (Some(from_entry), Some(to_entry)) =
                (self.graph.get_node(from), self.graph.get_node(to))
        {
            let from_lang = self.graph.files().language_for_file(from_entry.file);
            let to_lang = self.graph.files().language_for_file(to_entry.file);
            if from_lang == to_lang {
                base_color
            } else {
                "red"
            }
        } else {
            base_color
        };

        let label = if self.config.show_edge_labels {
            edge_label(kind, self.graph.strings())
        } else {
            String::new()
        };

        let from_key = format!("n{}", from.index());
        let to_key = format!("n{}", to.index());

        if label.is_empty() {
            writeln!(
                dot,
                "  \"{from_key}\" -> \"{to_key}\" [style=\"{style}\", color=\"{color}\"];"
            )
            .expect("write to String never fails");
        } else {
            writeln!(
                dot,
                "  \"{from_key}\" -> \"{to_key}\" [style=\"{style}\", color=\"{color}\", label=\"{}\"];",
                escape_dot(&label)
            )
            .expect("write to String never fails");
        }
    }
}

// ============================================================================
// D2 Exporter
// ============================================================================

/// Configuration for D2 export.
#[derive(Debug, Clone)]
pub struct D2Config {
    /// Filter to specific languages.
    pub filter_languages: HashSet<Language>,
    /// Filter to specific edge kinds.
    pub filter_edges: HashSet<EdgeFilter>,
    /// Restrict rendering to a specific set of node IDs. When `Some`, only these
    /// nodes (and edges between them) are included in the output.
    pub filter_node_ids: Option<HashSet<NodeId>>,
    /// Highlight cross-language edges.
    pub highlight_cross_language: bool,
    /// Show node details.
    pub show_details: bool,
    /// Show edge labels.
    pub show_edge_labels: bool,
    /// Graph direction.
    pub direction: Direction,
}

impl Default for D2Config {
    fn default() -> Self {
        Self {
            filter_languages: HashSet::new(),
            filter_edges: HashSet::new(),
            filter_node_ids: None,
            highlight_cross_language: false,
            show_details: true,
            show_edge_labels: true,
            direction: Direction::LeftToRight,
        }
    }
}

impl D2Config {
    /// Enable cross-language highlighting.
    #[must_use]
    pub fn with_cross_language_highlight(mut self, enabled: bool) -> Self {
        self.highlight_cross_language = enabled;
        self
    }

    /// Show node details.
    #[must_use]
    pub fn with_details(mut self, enabled: bool) -> Self {
        self.show_details = enabled;
        self
    }

    /// Show edge labels.
    #[must_use]
    pub fn with_edge_labels(mut self, enabled: bool) -> Self {
        self.show_edge_labels = enabled;
        self
    }

    /// Restrict output to the given set of node IDs. Pass `None` to clear any filter.
    #[must_use]
    pub fn with_filter_node_ids(mut self, ids: Option<HashSet<NodeId>>) -> Self {
        self.filter_node_ids = ids;
        self
    }
}

/// D2 format exporter for unified graph.
pub struct UnifiedD2Exporter<'a> {
    graph: &'a GraphSnapshot,
    config: D2Config,
}

impl<'a> UnifiedD2Exporter<'a> {
    /// Create a new exporter with default configuration.
    #[must_use]
    pub fn new(graph: &'a GraphSnapshot) -> Self {
        Self {
            graph,
            config: D2Config::default(),
        }
    }

    /// Create a new exporter with custom configuration.
    #[must_use]
    pub fn with_config(graph: &'a GraphSnapshot, config: D2Config) -> Self {
        Self { graph, config }
    }

    /// Export graph to D2 format.
    #[must_use]
    pub fn export(&self) -> String {
        let mut d2 = String::new();

        // Direction
        writeln!(d2, "direction: {}", self.config.direction.as_str())
            .expect("write to String never fails");
        d2.push('\n');

        // Collect visible nodes
        let visible_nodes: HashSet<NodeId> =
            if let Some(ref filter_ids) = self.config.filter_node_ids {
                filter_ids.clone()
            } else {
                self.graph
                    .iter_nodes()
                    .filter(|(id, entry)| self.should_include_node(*id, entry))
                    .map(|(id, _)| id)
                    .collect()
            };

        // Export nodes
        for node_id in &visible_nodes {
            if let Some(entry) = self.graph.get_node(*node_id) {
                self.export_node(&mut d2, *node_id, entry);
            }
        }

        d2.push('\n');

        // Export edges
        for (from, to, kind) in self.graph.iter_edges() {
            if !visible_nodes.contains(&from) || !visible_nodes.contains(&to) {
                continue;
            }

            if !self.edge_allowed(&kind) {
                continue;
            }

            self.export_edge(&mut d2, from, to, &kind);
        }

        d2
    }

    /// Check if a node should be included.
    fn should_include_node(&self, _node_id: NodeId, entry: &NodeEntry) -> bool {
        // Gate 0d iter-2 fix: never include unified losers in D2
        // visualization. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            return false;
        }
        if !self.config.filter_languages.is_empty() {
            if let Some(lang) = self.graph.files().language_for_file(entry.file) {
                if !self.config.filter_languages.contains(&lang) {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }

    /// Check if an edge kind is allowed.
    fn edge_allowed(&self, kind: &EdgeKind) -> bool {
        if self.config.filter_edges.is_empty() {
            return true;
        }
        self.config.filter_edges.iter().any(|f| f.matches(kind))
    }

    /// Export a single node to D2.
    fn export_node(&self, d2: &mut String, node_id: NodeId, entry: &NodeEntry) {
        let lang = self.graph.files().language_for_file(entry.file);
        let color = lang.map_or(default_language_color(), language_color);
        let shape = match entry.kind {
            NodeKind::Class | NodeKind::Struct => "class",
            NodeKind::Interface | NodeKind::Trait => "oval",
            NodeKind::Module => "package",
            _ => "rectangle",
        };

        let name = self
            .graph
            .strings()
            .resolve(entry.name)
            .unwrap_or_else(|| std::sync::Arc::from("?"));
        let qualified_name = entry
            .qualified_name
            .and_then(|id| self.graph.strings().resolve(id))
            .unwrap_or_else(|| std::sync::Arc::clone(&name));

        let label = if self.config.show_details {
            let file = self
                .graph
                .files()
                .resolve(entry.file)
                .map_or_else(|| "?".to_string(), |p| p.to_string_lossy().into_owned());
            format!("{} ({file}:{})", qualified_name.as_ref(), entry.start_line)
        } else {
            qualified_name.to_string()
        };

        let node_key = format!("n{}", node_id.index());
        let label = escape_d2(&label);

        writeln!(d2, "{node_key}: \"{label}\" {{").expect("write to String never fails");
        writeln!(d2, "  shape: {shape}").expect("write to String never fails");
        writeln!(d2, "  style.fill: \"{color}\"").expect("write to String never fails");
        writeln!(d2, "}}").expect("write to String never fails");
    }

    /// Export a single edge to D2.
    fn export_edge(&self, d2: &mut String, from: NodeId, to: NodeId, kind: &EdgeKind) {
        let (style, base_color) = edge_style(kind);
        let color = if self.config.highlight_cross_language
            && let (Some(from_entry), Some(to_entry)) =
                (self.graph.get_node(from), self.graph.get_node(to))
        {
            let from_lang = self.graph.files().language_for_file(from_entry.file);
            let to_lang = self.graph.files().language_for_file(to_entry.file);
            if from_lang == to_lang {
                base_color
            } else {
                "#ff0000"
            }
        } else {
            base_color
        };

        let from_key = format!("n{}", from.index());
        let to_key = format!("n{}", to.index());

        let arrow = match kind {
            EdgeKind::Inherits | EdgeKind::Implements => "<->",
            _ => "->",
        };

        if self.config.show_edge_labels {
            let label = edge_label(kind, self.graph.strings());
            if label.is_empty() {
                writeln!(d2, "{from_key} {arrow} {to_key}: {{")
                    .expect("write to String never fails");
            } else {
                writeln!(d2, "{from_key} {arrow} {to_key}: \"{label}\" {{")
                    .expect("write to String never fails");
            }
        } else {
            writeln!(d2, "{from_key} {arrow} {to_key}: {{").expect("write to String never fails");
        }

        let d2_style = match style {
            "dashed" => "stroke-dash: 3",
            "dotted" => "stroke-dash: 1",
            "bold" => "stroke-width: 3",
            _ => "",
        };

        if !d2_style.is_empty() {
            writeln!(d2, "  style.{d2_style}").expect("write to String never fails");
        }
        writeln!(d2, "  style.stroke: \"{color}\"").expect("write to String never fails");
        writeln!(d2, "}}").expect("write to String never fails");
    }
}

// ============================================================================
// Mermaid Exporter
// ============================================================================

/// Configuration for Mermaid export.
#[derive(Debug, Clone)]
pub struct MermaidConfig {
    /// Filter to specific languages.
    pub filter_languages: HashSet<Language>,
    /// Filter to specific edge kinds.
    pub filter_edges: HashSet<EdgeFilter>,
    /// Highlight cross-language edges.
    pub highlight_cross_language: bool,
    /// Show edge labels.
    pub show_edge_labels: bool,
    /// Graph direction.
    pub direction: Direction,
    /// Restrict rendering to a specific set of node IDs. When `Some`, only these
    /// nodes (and edges between them) are included in the output.
    pub filter_node_ids: Option<HashSet<NodeId>>,
}

impl Default for MermaidConfig {
    fn default() -> Self {
        Self {
            filter_languages: HashSet::new(),
            filter_edges: HashSet::new(),
            highlight_cross_language: false,
            show_edge_labels: true,
            direction: Direction::LeftToRight,
            filter_node_ids: None,
        }
    }
}

impl MermaidConfig {
    /// Enable cross-language highlighting.
    #[must_use]
    pub fn with_cross_language_highlight(mut self, enabled: bool) -> Self {
        self.highlight_cross_language = enabled;
        self
    }

    /// Show edge labels.
    #[must_use]
    pub fn with_edge_labels(mut self, enabled: bool) -> Self {
        self.show_edge_labels = enabled;
        self
    }

    /// Restrict output to the given set of node IDs. Pass `None` to clear any filter.
    #[must_use]
    pub fn with_filter_node_ids(mut self, ids: Option<HashSet<NodeId>>) -> Self {
        self.filter_node_ids = ids;
        self
    }
}

/// Mermaid format exporter for unified graph.
pub struct UnifiedMermaidExporter<'a> {
    graph: &'a GraphSnapshot,
    config: MermaidConfig,
}

impl<'a> UnifiedMermaidExporter<'a> {
    /// Create a new exporter with default configuration.
    #[must_use]
    pub fn new(graph: &'a GraphSnapshot) -> Self {
        Self {
            graph,
            config: MermaidConfig::default(),
        }
    }

    /// Create a new exporter with custom configuration.
    #[must_use]
    pub fn with_config(graph: &'a GraphSnapshot, config: MermaidConfig) -> Self {
        Self { graph, config }
    }

    /// Export graph to Mermaid format.
    #[must_use]
    pub fn export(&self) -> String {
        let mut mermaid = String::new();

        // Header
        writeln!(mermaid, "graph {}", self.config.direction.as_str())
            .expect("write to String never fails");

        // Collect visible nodes
        let visible_nodes: HashSet<NodeId> =
            if let Some(ref filter_ids) = self.config.filter_node_ids {
                filter_ids.clone()
            } else {
                self.graph
                    .iter_nodes()
                    .filter(|(id, entry)| self.should_include_node(*id, entry))
                    .map(|(id, _)| id)
                    .collect()
            };

        // Export nodes
        for node_id in &visible_nodes {
            if let Some(entry) = self.graph.get_node(*node_id) {
                self.export_node(&mut mermaid, *node_id, entry);
            }
        }

        // Export edges
        for (from, to, kind) in self.graph.iter_edges() {
            if !visible_nodes.contains(&from) || !visible_nodes.contains(&to) {
                continue;
            }

            if !self.edge_allowed(&kind) {
                continue;
            }

            self.export_edge(&mut mermaid, from, to, &kind);
        }

        mermaid
    }

    /// Check if a node should be included.
    fn should_include_node(&self, _node_id: NodeId, entry: &NodeEntry) -> bool {
        // Gate 0d iter-2 fix: never include unified losers in
        // Mermaid visualization. See `NodeEntry::is_unified_loser`.
        if entry.is_unified_loser() {
            return false;
        }
        if !self.config.filter_languages.is_empty() {
            if let Some(lang) = self.graph.files().language_for_file(entry.file) {
                if !self.config.filter_languages.contains(&lang) {
                    return false;
                }
            } else {
                return false;
            }
        }
        true
    }

    /// Check if an edge kind is allowed.
    fn edge_allowed(&self, kind: &EdgeKind) -> bool {
        if self.config.filter_edges.is_empty() {
            return true;
        }
        self.config.filter_edges.iter().any(|f| f.matches(kind))
    }

    /// Export a single node to Mermaid.
    fn export_node(&self, mermaid: &mut String, node_id: NodeId, entry: &NodeEntry) {
        let name = self
            .graph
            .strings()
            .resolve(entry.name)
            .unwrap_or_else(|| std::sync::Arc::from("?"));
        let qualified_name = entry
            .qualified_name
            .and_then(|id| self.graph.strings().resolve(id))
            .unwrap_or_else(|| std::sync::Arc::clone(&name));

        let node_key = format!("n{}", node_id.index());
        let label = Self::escape_mermaid(&qualified_name);

        // Node shapes in Mermaid
        let (open, close) = match entry.kind {
            NodeKind::Class | NodeKind::Struct => ("[[", "]]"),
            NodeKind::Interface | NodeKind::Trait => ("([", "])"),
            NodeKind::Module => ("{{", "}}"),
            _ => ("[", "]"),
        };

        writeln!(mermaid, "    {node_key}{open}\"{label}\"{close}")
            .expect("write to String never fails");
    }

    /// Export a single edge to Mermaid.
    fn export_edge(&self, mermaid: &mut String, from: NodeId, to: NodeId, kind: &EdgeKind) {
        let from_key = format!("n{}", from.index());
        let to_key = format!("n{}", to.index());

        let arrow = match kind {
            EdgeKind::Imports { .. } | EdgeKind::Exports { .. } => "-.->",
            EdgeKind::Calls { is_async: true, .. } => "==>",
            _ => "-->",
        };

        if self.config.show_edge_labels {
            let label = edge_label(kind, self.graph.strings());
            if label.is_empty() {
                writeln!(mermaid, "    {from_key} {arrow} {to_key}")
                    .expect("write to String never fails");
            } else {
                let label = Self::escape_mermaid(&label);
                writeln!(mermaid, "    {from_key} {arrow}|\"{label}\"| {to_key}")
                    .expect("write to String never fails");
            }
        } else {
            writeln!(mermaid, "    {from_key} {arrow} {to_key}")
                .expect("write to String never fails");
        }
    }

    /// Escape string for Mermaid.
    fn escape_mermaid(s: &str) -> String {
        s.replace('"', "#quot;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }
}

// ============================================================================
// JSON Exporter
// ============================================================================

/// Configuration for JSON export.
#[derive(Debug, Clone, Default)]
pub struct JsonConfig {
    /// Include node details (signature, doc).
    pub include_details: bool,
    /// Include edge metadata.
    pub include_edge_metadata: bool,
}

impl JsonConfig {
    /// Include node details.
    #[must_use]
    pub fn with_details(mut self, enabled: bool) -> Self {
        self.include_details = enabled;
        self
    }

    /// Include edge metadata.
    #[must_use]
    pub fn with_edge_metadata(mut self, enabled: bool) -> Self {
        self.include_edge_metadata = enabled;
        self
    }
}

/// JSON format exporter for unified graph.
pub struct UnifiedJsonExporter<'a> {
    graph: &'a GraphSnapshot,
    config: JsonConfig,
}

impl<'a> UnifiedJsonExporter<'a> {
    /// Create a new exporter with default configuration.
    #[must_use]
    pub fn new(graph: &'a GraphSnapshot) -> Self {
        Self {
            graph,
            config: JsonConfig::default(),
        }
    }

    /// Create a new exporter with custom configuration.
    #[must_use]
    pub fn with_config(graph: &'a GraphSnapshot, config: JsonConfig) -> Self {
        Self { graph, config }
    }

    /// Export graph to JSON.
    #[must_use]
    pub fn export(&self) -> serde_json::Value {
        let nodes = self.export_nodes();
        let edges = self.export_edges();

        serde_json::json!({
            "nodes": nodes,
            "edges": edges,
            "metadata": {
                "node_count": nodes.len(),
                "edge_count": edges.len(),
            }
        })
    }

    fn export_nodes(&self) -> Vec<serde_json::Value> {
        // Gate 0d iter-2 fix: skip unified losers from JSON
        // visualization export. See `NodeEntry::is_unified_loser`.
        self.graph
            .iter_nodes()
            .filter(|(_, entry)| !entry.is_unified_loser())
            .map(|(node_id, entry)| self.export_node(node_id, entry))
            .collect()
    }

    fn export_node(&self, node_id: NodeId, entry: &NodeEntry) -> serde_json::Value {
        let name = self
            .graph
            .strings()
            .resolve(entry.name)
            .map_or_else(|| "?".to_string(), |s| s.to_string());
        let qualified_name = entry
            .qualified_name
            .and_then(|id| self.graph.strings().resolve(id))
            .map_or_else(|| name.clone(), |s| s.to_string());
        let file = self
            .graph
            .files()
            .resolve(entry.file)
            .map_or_else(|| "?".to_string(), |p| p.to_string_lossy().into_owned());
        let lang = self
            .graph
            .files()
            .language_for_file(entry.file)
            .map_or_else(|| "Unknown".to_string(), |l| format!("{l:?}"));

        let mut node = serde_json::json!({
            "id": format!("n{}", node_id.index()),
            "name": name,
            "qualified_name": qualified_name,
            "kind": format!("{:?}", entry.kind),
            "file": file,
            "language": lang,
            "line": entry.start_line,
        });

        if self.config.include_details {
            self.append_node_details(entry, &mut node);
        }

        node
    }

    fn append_node_details(&self, entry: &NodeEntry, node: &mut serde_json::Value) {
        if let Some(sig_id) = entry.signature
            && let Some(sig) = self.graph.strings().resolve(sig_id)
        {
            node["signature"] = serde_json::Value::String(sig.to_string());
        }
        if let Some(doc_id) = entry.doc
            && let Some(doc) = self.graph.strings().resolve(doc_id)
        {
            node["doc"] = serde_json::Value::String(doc.to_string());
        }
        if let Some(vis_id) = entry.visibility
            && let Some(vis) = self.graph.strings().resolve(vis_id)
        {
            node["visibility"] = serde_json::Value::String(vis.to_string());
        }
        node["is_async"] = serde_json::Value::Bool(entry.is_async);
        node["is_static"] = serde_json::Value::Bool(entry.is_static);
    }

    fn export_edges(&self) -> Vec<serde_json::Value> {
        self.graph
            .iter_edges()
            .map(|(from, to, kind)| self.export_edge(from, to, &kind))
            .collect()
    }

    fn export_edge(&self, from: NodeId, to: NodeId, kind: &EdgeKind) -> serde_json::Value {
        let from_key = format!("n{}", from.index());
        let to_key = format!("n{}", to.index());

        let mut edge = serde_json::json!({
            "from": from_key,
            "to": to_key,
            "kind": Self::edge_kind_name(kind),
        });

        if self.config.include_edge_metadata {
            self.append_edge_metadata(kind, &mut edge);
        }

        edge
    }

    fn append_edge_metadata(&self, kind: &EdgeKind, edge: &mut serde_json::Value) {
        match kind {
            EdgeKind::Calls {
                argument_count,
                is_async,
            } => {
                edge["argument_count"] = serde_json::Value::Number((*argument_count).into());
                edge["is_async"] = serde_json::Value::Bool(*is_async);
            }
            EdgeKind::Imports { alias, is_wildcard } => {
                edge["is_wildcard"] = serde_json::Value::Bool(*is_wildcard);
                if let Some(alias_id) = alias
                    && let Some(alias_str) = self.graph.strings().resolve(*alias_id)
                {
                    edge["alias"] = serde_json::Value::String(alias_str.to_string());
                }
            }
            EdgeKind::Exports {
                kind: export_kind,
                alias,
            } => {
                edge["export_kind"] = serde_json::Value::String(format!("{export_kind:?}"));
                if let Some(alias_id) = alias
                    && let Some(alias_str) = self.graph.strings().resolve(*alias_id)
                {
                    edge["alias"] = serde_json::Value::String(alias_str.to_string());
                }
            }
            EdgeKind::HttpRequest { method, url } => {
                edge["method"] = serde_json::Value::String(method.as_str().to_string());
                if let Some(url_id) = url
                    && let Some(url_str) = self.graph.strings().resolve(*url_id)
                {
                    edge["url"] = serde_json::Value::String(url_str.to_string());
                }
            }
            _ => {}
        }
    }

    /// Get edge kind name as string.
    fn edge_kind_name(kind: &EdgeKind) -> &'static str {
        match kind {
            EdgeKind::Defines => "defines",
            EdgeKind::Contains => "contains",
            EdgeKind::Calls { .. } => "calls",
            EdgeKind::References => "references",
            EdgeKind::Imports { .. } => "imports",
            EdgeKind::Exports { .. } => "exports",
            EdgeKind::TypeOf { .. } => "type_of",
            EdgeKind::Inherits => "inherits",
            EdgeKind::Implements => "implements",
            EdgeKind::FfiCall { .. } => "ffi_call",
            EdgeKind::HttpRequest { .. } => "http_request",
            EdgeKind::GrpcCall { .. } => "grpc_call",
            EdgeKind::WebAssemblyCall => "wasm_call",
            EdgeKind::DbQuery { .. } => "db_query",
            EdgeKind::TableRead { .. } => "table_read",
            EdgeKind::TableWrite { .. } => "table_write",
            EdgeKind::TriggeredBy { .. } => "triggered_by",
            EdgeKind::MessageQueue { .. } => "message_queue",
            EdgeKind::WebSocket { .. } => "websocket",
            EdgeKind::GraphQLOperation { .. } => "graphql_operation",
            EdgeKind::ProcessExec { .. } => "process_exec",
            EdgeKind::FileIpc { .. } => "file_ipc",
            EdgeKind::ProtocolCall { .. } => "protocol_call",
            // Rust-specific edge kinds
            EdgeKind::LifetimeConstraint { .. } => "lifetime_constraint",
            EdgeKind::TraitMethodBinding { .. } => "trait_method_binding",
            EdgeKind::MacroExpansion { .. } => "macro_expansion",
            // JVM Classpath (Track C)
            EdgeKind::GenericBound => "generic_bound",
            EdgeKind::AnnotatedWith => "annotated_with",
            EdgeKind::AnnotationParam => "annotation_param",
            EdgeKind::LambdaCaptures => "lambda_captures",
            EdgeKind::ModuleExports => "module_exports",
            EdgeKind::ModuleRequires => "module_requires",
            EdgeKind::ModuleOpens => "module_opens",
            EdgeKind::ModuleProvides => "module_provides",
            EdgeKind::TypeArgument => "type_argument",
            EdgeKind::ExtensionReceiver => "extension_receiver",
            EdgeKind::CompanionOf => "companion_of",
            EdgeKind::SealedPermit => "sealed_permit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== Direction tests =====

    #[test]
    fn test_direction_as_str() {
        assert_eq!(Direction::LeftToRight.as_str(), "LR");
        assert_eq!(Direction::TopToBottom.as_str(), "TB");
    }

    #[test]
    fn test_direction_default() {
        assert_eq!(Direction::default(), Direction::LeftToRight);
    }

    // ===== EdgeFilter tests =====

    #[test]
    fn test_edge_filter_matches_calls() {
        assert!(EdgeFilter::Calls.matches(&EdgeKind::Calls {
            argument_count: 0,
            is_async: false
        }));
        assert!(EdgeFilter::Calls.matches(&EdgeKind::Calls {
            argument_count: 5,
            is_async: true
        }));
        assert!(!EdgeFilter::Calls.matches(&EdgeKind::References));
    }

    #[test]
    fn test_edge_filter_matches_imports() {
        assert!(EdgeFilter::Imports.matches(&EdgeKind::Imports {
            alias: None,
            is_wildcard: false
        }));
        assert!(EdgeFilter::Imports.matches(&EdgeKind::Imports {
            alias: None,
            is_wildcard: true
        }));
        assert!(!EdgeFilter::Imports.matches(&EdgeKind::Exports {
            kind: crate::graph::unified::edge::ExportKind::Direct,
            alias: None
        }));
    }

    #[test]
    fn test_edge_filter_matches_exports() {
        assert!(EdgeFilter::Exports.matches(&EdgeKind::Exports {
            kind: crate::graph::unified::edge::ExportKind::Direct,
            alias: None
        }));
        assert!(!EdgeFilter::Exports.matches(&EdgeKind::Imports {
            alias: None,
            is_wildcard: false
        }));
    }

    #[test]
    fn test_edge_filter_matches_references() {
        assert!(EdgeFilter::References.matches(&EdgeKind::References));
        assert!(!EdgeFilter::References.matches(&EdgeKind::Calls {
            argument_count: 0,
            is_async: false
        }));
    }

    #[test]
    fn test_edge_filter_matches_inheritance() {
        assert!(EdgeFilter::Inherits.matches(&EdgeKind::Inherits));
        assert!(EdgeFilter::Implements.matches(&EdgeKind::Implements));
        assert!(!EdgeFilter::Inherits.matches(&EdgeKind::Implements));
        assert!(!EdgeFilter::Implements.matches(&EdgeKind::Inherits));
    }

    #[test]
    fn test_edge_filter_matches_cross_language() {
        assert!(EdgeFilter::FfiCall.matches(&EdgeKind::FfiCall {
            convention: crate::graph::unified::edge::FfiConvention::C
        }));
        assert!(EdgeFilter::HttpRequest.matches(&EdgeKind::HttpRequest {
            method: crate::graph::unified::edge::HttpMethod::Get,
            url: None
        }));
        assert!(EdgeFilter::DbQuery.matches(&EdgeKind::DbQuery {
            query_type: crate::graph::unified::edge::DbQueryType::Select,
            table: None
        }));
    }

    // ===== Language color tests =====

    #[test]
    fn test_language_color_common_languages() {
        assert_eq!(language_color(Language::Rust), "#dea584");
        assert_eq!(language_color(Language::JavaScript), "#f7df1e");
        assert_eq!(language_color(Language::TypeScript), "#3178c6");
        assert_eq!(language_color(Language::Python), "#3572A5");
        assert_eq!(language_color(Language::Go), "#00ADD8");
        assert_eq!(language_color(Language::Java), "#b07219");
    }

    #[test]
    fn test_language_color_all_languages() {
        // Ensure all languages have colors (no panic)
        let languages = [
            Language::Rust,
            Language::JavaScript,
            Language::TypeScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::Ruby,
            Language::Php,
            Language::Cpp,
            Language::C,
            Language::Swift,
            Language::Kotlin,
            Language::Scala,
            Language::Sql,
            Language::Plsql,
            Language::Shell,
            Language::Lua,
            Language::Perl,
            Language::Dart,
            Language::Groovy,
            Language::Http,
            Language::Css,
            Language::Elixir,
            Language::R,
            Language::Haskell,
            Language::Html,
            Language::Svelte,
            Language::Vue,
            Language::Zig,
            Language::Terraform,
            Language::Puppet,
            Language::Apex,
            Language::Abap,
            Language::ServiceNow,
            Language::CSharp,
        ];
        for lang in languages {
            let color = language_color(lang);
            assert!(color.starts_with('#'), "Color for {lang:?} should be hex");
        }
    }

    #[test]
    fn test_default_language_color() {
        assert_eq!(default_language_color(), "#cccccc");
    }

    // ===== Node shape tests =====

    #[test]
    fn test_node_shape_class_types() {
        assert_eq!(node_shape(&NodeKind::Class), "component");
        assert_eq!(node_shape(&NodeKind::Struct), "component");
    }

    #[test]
    fn test_node_shape_interface_types() {
        assert_eq!(node_shape(&NodeKind::Interface), "ellipse");
        assert_eq!(node_shape(&NodeKind::Trait), "ellipse");
    }

    #[test]
    fn test_node_shape_module() {
        assert_eq!(node_shape(&NodeKind::Module), "folder");
    }

    #[test]
    fn test_node_shape_variables() {
        assert_eq!(node_shape(&NodeKind::Variable), "note");
        assert_eq!(node_shape(&NodeKind::Constant), "note");
    }

    #[test]
    fn test_node_shape_enums() {
        assert_eq!(node_shape(&NodeKind::Enum), "hexagon");
        assert_eq!(node_shape(&NodeKind::EnumVariant), "hexagon");
    }

    #[test]
    fn test_node_shape_special() {
        assert_eq!(node_shape(&NodeKind::Type), "diamond");
        assert_eq!(node_shape(&NodeKind::Macro), "parallelogram");
    }

    #[test]
    fn test_node_shape_default() {
        assert_eq!(node_shape(&NodeKind::Function), "box");
        assert_eq!(node_shape(&NodeKind::Method), "box");
    }

    // ===== Edge style tests =====

    #[test]
    fn test_edge_style_calls() {
        let (style, color) = edge_style(&EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        });
        assert_eq!(style, "solid");
        assert_eq!(color, "#333333");
    }

    #[test]
    fn test_edge_style_imports_exports() {
        let (style, color) = edge_style(&EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        });
        assert_eq!(style, "dashed");
        assert_eq!(color, "#0066cc");

        let (style, color) = edge_style(&EdgeKind::Exports {
            kind: crate::graph::unified::edge::ExportKind::Direct,
            alias: None,
        });
        assert_eq!(style, "dashed");
        assert_eq!(color, "#00cc66");
    }

    #[test]
    fn test_edge_style_references() {
        let (style, color) = edge_style(&EdgeKind::References);
        assert_eq!(style, "dotted");
        assert_eq!(color, "#666666");
    }

    #[test]
    fn test_edge_style_inheritance() {
        let (style, color) = edge_style(&EdgeKind::Inherits);
        assert_eq!(style, "solid");
        assert_eq!(color, "#990099");

        let (style, color) = edge_style(&EdgeKind::Implements);
        assert_eq!(style, "dashed");
        assert_eq!(color, "#990099");
    }

    #[test]
    fn test_edge_style_cross_language() {
        let (style, color) = edge_style(&EdgeKind::FfiCall {
            convention: crate::graph::unified::edge::FfiConvention::C,
        });
        assert_eq!(style, "bold");
        assert_eq!(color, "#ff6600");

        let (style, color) = edge_style(&EdgeKind::HttpRequest {
            method: crate::graph::unified::edge::HttpMethod::Get,
            url: None,
        });
        assert_eq!(style, "bold");
        assert_eq!(color, "#cc0000");

        let (style, color) = edge_style(&EdgeKind::DbQuery {
            query_type: crate::graph::unified::edge::DbQueryType::Select,
            table: None,
        });
        assert_eq!(style, "bold");
        assert_eq!(color, "#009900");
    }

    // ===== Escape function tests =====

    #[test]
    fn test_escape_dot_basic() {
        assert_eq!(escape_dot("hello"), "hello");
        assert_eq!(escape_dot("hello world"), "hello world");
    }

    #[test]
    fn test_escape_dot_quotes() {
        assert_eq!(escape_dot("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_dot("\"quoted\""), "\\\"quoted\\\"");
    }

    #[test]
    fn test_escape_dot_newlines() {
        assert_eq!(escape_dot("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_dot("a\nb\nc"), "a\\nb\\nc");
    }

    #[test]
    fn test_escape_dot_backslashes() {
        assert_eq!(escape_dot("path\\to\\file"), "path\\\\to\\\\file");
    }

    #[test]
    fn test_escape_d2_basic() {
        assert_eq!(escape_d2("hello"), "hello");
        assert_eq!(escape_d2("hello world"), "hello world");
    }

    #[test]
    fn test_escape_d2_quotes_and_newlines() {
        // D2 escape only handles \, ", and newlines
        assert_eq!(escape_d2("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_d2("line1\nline2"), "line1 line2");
        assert_eq!(escape_d2("path\\to\\file"), "path\\\\to\\\\file");
    }

    // ===== Config builder tests =====

    #[test]
    fn test_dot_config_default() {
        let config = DotConfig::default();
        assert_eq!(config.direction, Direction::LeftToRight);
        assert!(!config.highlight_cross_language);
        assert!(config.show_details); // Default is true
        assert!(config.show_edge_labels); // Default is true
        assert!(config.filter_node_ids.is_none());
        assert!(config.filter_languages.is_empty());
        assert!(config.filter_edges.is_empty());
        assert!(config.filter_files.is_empty());
    }

    #[test]
    fn test_dot_config_builder() {
        let config = DotConfig::default()
            .with_direction(Direction::TopToBottom)
            .with_cross_language_highlight(true)
            .with_details(true);
        assert_eq!(config.direction, Direction::TopToBottom);
        assert!(config.highlight_cross_language);
        assert!(config.show_details);

        // Verify with_filter_node_ids sets the field correctly
        let mut ids = HashSet::new();
        ids.insert(NodeId::new(0, 0));
        let config_with_filter = DotConfig::default().with_filter_node_ids(Some(ids.clone()));
        assert!(config_with_filter.filter_node_ids.is_some());
        assert_eq!(config_with_filter.filter_node_ids.as_ref().unwrap(), &ids);

        let config_cleared = config_with_filter.with_filter_node_ids(None);
        assert!(config_cleared.filter_node_ids.is_none());
    }

    #[test]
    fn test_d2_config_default() {
        let config = D2Config::default();
        assert_eq!(config.direction, Direction::LeftToRight);
        assert!(!config.highlight_cross_language);
        assert!(config.show_details); // Default is true
        assert!(config.show_edge_labels); // Default is true
        assert!(config.filter_node_ids.is_none());
        assert!(config.filter_languages.is_empty());
        assert!(config.filter_edges.is_empty());
    }

    #[test]
    fn test_d2_config_builder() {
        let config = D2Config::default()
            .with_cross_language_highlight(true)
            .with_details(true)
            .with_edge_labels(true);
        assert!(config.highlight_cross_language);
        assert!(config.show_details);
        assert!(config.show_edge_labels);

        // Verify with_filter_node_ids sets the field correctly
        let mut ids = HashSet::new();
        ids.insert(NodeId::new(0, 0));
        let config_with_filter = D2Config::default().with_filter_node_ids(Some(ids.clone()));
        assert!(config_with_filter.filter_node_ids.is_some());
        assert_eq!(config_with_filter.filter_node_ids.as_ref().unwrap(), &ids);

        let config_cleared = config_with_filter.with_filter_node_ids(None);
        assert!(config_cleared.filter_node_ids.is_none());
    }

    #[test]
    fn test_mermaid_config_default() {
        let config = MermaidConfig::default();
        assert_eq!(config.direction, Direction::LeftToRight);
        assert!(!config.highlight_cross_language);
        assert!(config.show_edge_labels); // Default is true
        assert!(config.filter_node_ids.is_none());
        assert!(config.filter_languages.is_empty());
        assert!(config.filter_edges.is_empty());
    }

    #[test]
    fn test_mermaid_config_builder() {
        let config = MermaidConfig::default()
            .with_cross_language_highlight(true)
            .with_edge_labels(false);
        assert!(config.highlight_cross_language);
        assert!(!config.show_edge_labels);

        // Verify with_filter_node_ids sets the field correctly
        let mut ids = HashSet::new();
        ids.insert(NodeId::new(0, 0));
        let config_with_filter = MermaidConfig::default().with_filter_node_ids(Some(ids.clone()));
        assert!(config_with_filter.filter_node_ids.is_some());
        assert_eq!(config_with_filter.filter_node_ids.as_ref().unwrap(), &ids);

        let config_cleared = config_with_filter.with_filter_node_ids(None);
        assert!(config_cleared.filter_node_ids.is_none());
    }

    #[test]
    fn test_json_config_builder() {
        let config = JsonConfig::default()
            .with_details(true)
            .with_edge_metadata(true);
        assert!(config.include_details);
        assert!(config.include_edge_metadata);
    }

    // ============================================================================
    // Exporter tests with test graph
    // ============================================================================

    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::BidirectionalEdgeStore;
    use crate::graph::unified::storage::NodeEntry;
    use crate::graph::unified::storage::arena::NodeArena;
    use crate::graph::unified::storage::indices::AuxiliaryIndices;
    use crate::graph::unified::storage::interner::StringInterner;
    use crate::graph::unified::storage::registry::FileRegistry;
    use std::path::Path;

    /// Create a minimal test graph with nodes and edges for exporter testing.
    fn create_test_graph_for_export() -> CodeGraph {
        let mut nodes = NodeArena::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let edges = BidirectionalEdgeStore::new();
        let indices = AuxiliaryIndices::new();

        // Register file with language
        let file_id = files
            .register_with_language(Path::new("src/main.rs"), Some(Language::Rust))
            .unwrap();

        // Intern strings
        let name_main = strings.intern("main").unwrap();
        let name_helper = strings.intern("helper").unwrap();
        let qname_main = strings.intern("app::main").unwrap();
        let qname_helper = strings.intern("app::helper").unwrap();
        let sig = strings.intern("fn main()").unwrap();

        // Add nodes
        let main_entry = NodeEntry {
            kind: NodeKind::Function,
            name: name_main,
            file: file_id,
            start_byte: 0,
            end_byte: 100,
            start_line: 1,
            start_column: 0,
            end_line: 10,
            end_column: 1,
            signature: Some(sig),
            doc: None,
            qualified_name: Some(qname_main),
            visibility: None,
            is_async: false,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let main_id = nodes.alloc(main_entry).unwrap();

        let helper_entry = NodeEntry {
            kind: NodeKind::Function,
            name: name_helper,
            file: file_id,
            start_byte: 100,
            end_byte: 200,
            start_line: 11,
            start_column: 0,
            end_line: 20,
            end_column: 1,
            signature: None,
            doc: None,
            qualified_name: Some(qname_helper),
            visibility: None,
            is_async: true,
            is_static: false,
            is_unsafe: false,
            body_hash: None,
        };
        let helper_id = nodes.alloc(helper_entry).unwrap();

        // Add call edge: main -> helper
        edges.add_edge(
            main_id,
            helper_id,
            EdgeKind::Calls {
                argument_count: 2,
                is_async: false,
            },
            file_id,
        );

        CodeGraph::from_components(
            nodes,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        )
    }

    // ===== DOT Exporter tests =====

    #[test]
    fn test_unified_dot_exporter_basic() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedDotExporter::new(&snapshot);
        let output = exporter.export();

        // Verify DOT structure
        assert!(output.starts_with("digraph CodeGraph {"));
        assert!(output.ends_with("}\n"));
        assert!(output.contains("rankdir=LR"));
    }

    #[test]
    fn test_unified_dot_exporter_with_config() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let config = DotConfig::default()
            .with_direction(Direction::TopToBottom)
            .with_cross_language_highlight(true)
            .with_details(true);
        let exporter = UnifiedDotExporter::with_config(&snapshot, config);
        let output = exporter.export();

        assert!(output.contains("rankdir=TB"));
    }

    #[test]
    fn test_unified_dot_exporter_contains_nodes() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedDotExporter::new(&snapshot);
        let output = exporter.export();

        // Should contain node definitions
        assert!(output.contains("n0"), "Should contain first node");
        assert!(output.contains("n1"), "Should contain second node");
        // Should contain node labels
        assert!(
            output.contains("main") || output.contains("app::main"),
            "Should contain main function name"
        );
    }

    #[test]
    fn test_unified_dot_exporter_contains_edges() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedDotExporter::new(&snapshot);
        let output = exporter.export();

        // Should contain edge definitions (n0 -> n1 format)
        assert!(output.contains("->"), "Should contain edge arrow");
    }

    #[test]
    fn test_unified_dot_exporter_empty_graph() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let exporter = UnifiedDotExporter::new(&snapshot);
        let output = exporter.export();

        assert!(output.starts_with("digraph CodeGraph {"));
        assert!(output.ends_with("}\n"));
        // Empty graph should have no node definitions
        assert!(!output.contains("n0"));
    }

    // ===== D2 Exporter tests =====

    #[test]
    fn test_unified_d2_exporter_basic() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedD2Exporter::new(&snapshot);
        let output = exporter.export();

        // Verify D2 structure
        assert!(output.contains("direction: LR"));
    }

    #[test]
    fn test_unified_d2_exporter_with_config() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let config = D2Config::default().with_cross_language_highlight(true);
        let exporter = UnifiedD2Exporter::with_config(&snapshot, config);
        let output = exporter.export();

        assert!(output.contains("direction:"));
    }

    #[test]
    fn test_unified_d2_exporter_contains_nodes() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedD2Exporter::new(&snapshot);
        let output = exporter.export();

        // D2 nodes are defined with key: "label" { style }
        assert!(
            output.contains("n0:"),
            "Should contain first node definition"
        );
        assert!(
            output.contains("n1:"),
            "Should contain second node definition"
        );
    }

    #[test]
    fn test_unified_d2_exporter_contains_edges() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedD2Exporter::new(&snapshot);
        let output = exporter.export();

        // D2 edges use -> or <-> format
        assert!(
            output.contains("->") || output.contains("<->"),
            "Should contain edge"
        );
    }

    // ===== Mermaid Exporter tests =====

    #[test]
    fn test_unified_mermaid_exporter_basic() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedMermaidExporter::new(&snapshot);
        let output = exporter.export();

        // Verify Mermaid structure
        assert!(output.starts_with("graph LR"));
    }

    #[test]
    fn test_unified_mermaid_exporter_with_config() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let config = MermaidConfig::default()
            .with_cross_language_highlight(true)
            .with_edge_labels(false);
        let exporter = UnifiedMermaidExporter::with_config(&snapshot, config);
        let output = exporter.export();

        assert!(output.starts_with("graph LR"));
    }

    #[test]
    fn test_unified_mermaid_exporter_contains_nodes() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedMermaidExporter::new(&snapshot);
        let output = exporter.export();

        // Mermaid nodes use node[label] or node["label"] format
        assert!(output.contains("n0"), "Should contain first node");
        assert!(output.contains("n1"), "Should contain second node");
    }

    #[test]
    fn test_unified_mermaid_exporter_contains_edges() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedMermaidExporter::new(&snapshot);
        let output = exporter.export();

        // Mermaid edges use -->, ==>, or -.->
        assert!(
            output.contains("-->") || output.contains("==>") || output.contains("-.->"),
            "Should contain edge"
        );
    }

    // ===== JSON Exporter tests =====

    #[test]
    fn test_unified_json_exporter_basic() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedJsonExporter::new(&snapshot);
        let output = exporter.export();

        // Verify JSON structure
        assert!(output.is_object());
        assert!(output.get("nodes").is_some());
        assert!(output.get("edges").is_some());
        assert!(output.get("metadata").is_some());
    }

    #[test]
    fn test_unified_json_exporter_node_count() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedJsonExporter::new(&snapshot);
        let output = exporter.export();

        let nodes = output.get("nodes").unwrap().as_array().unwrap();
        assert_eq!(nodes.len(), 2, "Should have 2 nodes");
    }

    #[test]
    fn test_unified_json_exporter_edge_count() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedJsonExporter::new(&snapshot);
        let output = exporter.export();

        let edges = output.get("edges").unwrap().as_array().unwrap();
        assert_eq!(edges.len(), 1, "Should have 1 edge");
    }

    #[test]
    fn test_unified_json_exporter_metadata() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let exporter = UnifiedJsonExporter::new(&snapshot);
        let output = exporter.export();

        let metadata = output.get("metadata").unwrap();
        assert_eq!(
            metadata.get("node_count").unwrap().as_u64().unwrap(),
            2,
            "Metadata should report 2 nodes"
        );
        assert_eq!(
            metadata.get("edge_count").unwrap().as_u64().unwrap(),
            1,
            "Metadata should report 1 edge"
        );
    }

    #[test]
    fn test_unified_json_exporter_with_details() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let config = JsonConfig::default().with_details(true);
        let exporter = UnifiedJsonExporter::with_config(&snapshot, config);
        let output = exporter.export();

        let nodes = output.get("nodes").unwrap().as_array().unwrap();
        // First node (main) has a signature
        let main_node = &nodes[0];
        assert!(
            main_node.get("signature").is_some(),
            "Node should have signature when details enabled"
        );
    }

    #[test]
    fn test_unified_json_exporter_with_edge_metadata() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();
        let config = JsonConfig::default().with_edge_metadata(true);
        let exporter = UnifiedJsonExporter::with_config(&snapshot, config);
        let output = exporter.export();

        let edges = output.get("edges").unwrap().as_array().unwrap();
        let edge = &edges[0];
        // Call edge should have argument_count when metadata enabled
        assert!(
            edge.get("argument_count").is_some(),
            "Edge should have argument_count metadata"
        );
    }

    #[test]
    fn test_unified_json_exporter_empty_graph() {
        let graph = CodeGraph::new();
        let snapshot = graph.snapshot();
        let exporter = UnifiedJsonExporter::new(&snapshot);
        let output = exporter.export();

        let nodes = output.get("nodes").unwrap().as_array().unwrap();
        let edges = output.get("edges").unwrap().as_array().unwrap();
        assert!(nodes.is_empty(), "Empty graph should have no nodes");
        assert!(edges.is_empty(), "Empty graph should have no edges");
    }

    // ===== edge_label function tests =====

    #[test]
    fn test_edge_label_calls() {
        let strings = StringInterner::new();
        let label = edge_label(
            &EdgeKind::Calls {
                argument_count: 3,
                is_async: false,
            },
            &strings,
        );
        assert_eq!(label, "call(3)");
    }

    #[test]
    fn test_edge_label_async_calls() {
        let strings = StringInterner::new();
        let label = edge_label(
            &EdgeKind::Calls {
                argument_count: 2,
                is_async: true,
            },
            &strings,
        );
        assert_eq!(label, "async call(2)");
    }

    #[test]
    fn test_edge_label_imports_simple() {
        let strings = StringInterner::new();
        let label = edge_label(
            &EdgeKind::Imports {
                alias: None,
                is_wildcard: false,
            },
            &strings,
        );
        assert_eq!(label, "import");
    }

    #[test]
    fn test_edge_label_imports_wildcard() {
        let strings = StringInterner::new();
        let label = edge_label(
            &EdgeKind::Imports {
                alias: None,
                is_wildcard: true,
            },
            &strings,
        );
        assert_eq!(label, "import *");
    }

    #[test]
    fn test_edge_label_references() {
        let strings = StringInterner::new();
        let label = edge_label(&EdgeKind::References, &strings);
        assert_eq!(label, "ref");
    }

    #[test]
    fn test_edge_label_inherits() {
        let strings = StringInterner::new();
        let label = edge_label(&EdgeKind::Inherits, &strings);
        assert_eq!(label, "extends");
    }

    #[test]
    fn test_edge_label_implements() {
        let strings = StringInterner::new();
        let label = edge_label(&EdgeKind::Implements, &strings);
        assert_eq!(label, "implements");
    }

    // ===== MermaidConfig filter_node_ids tests =====

    #[test]
    fn test_mermaid_filter_node_ids_restricts_output() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();

        // Collect nodes in iteration order so we can pick a specific one.
        let all_nodes: Vec<NodeId> = snapshot.iter_nodes().map(|(id, _)| id).collect();
        assert!(
            all_nodes.len() >= 2,
            "test graph must have at least 2 nodes"
        );

        // Filter to contain only the first node in iteration order.
        let kept_id = all_nodes[0];
        let expected_key = format!("n{}", kept_id.index());
        // The excluded node key must NOT appear in the output.
        let excluded_key = format!("n{}", all_nodes[1].index());

        let mut filter = HashSet::new();
        filter.insert(kept_id);

        let config = MermaidConfig::default().with_filter_node_ids(Some(filter));
        let exporter = UnifiedMermaidExporter::with_config(&snapshot, config);
        let output = exporter.export();

        // Collect all node-definition lines (lines that start with `nN[`).
        let node_defs: Vec<&str> = output
            .lines()
            .filter(|l| l.trim_start().starts_with('n') && l.contains('['))
            .collect();

        assert_eq!(
            node_defs.len(),
            1,
            "filtered export should have exactly 1 node, got: {node_defs:?}"
        );

        // The single emitted node must be the one we kept.
        assert!(
            node_defs[0].trim_start().starts_with(&expected_key),
            "expected node key '{expected_key}' but got: {}",
            node_defs[0]
        );

        // The excluded node must not appear anywhere in the output.
        let excluded_present = output
            .lines()
            .any(|l| l.trim_start().starts_with(&excluded_key));
        assert!(
            !excluded_present,
            "excluded node key '{excluded_key}' must not appear in filtered output"
        );

        // With only one visible node, no edges can exist between visible nodes.
        let edge_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains("-->") || l.contains("---"))
            .collect();
        assert!(
            edge_lines.is_empty(),
            "no edges should appear when only one node is visible, got: {edge_lines:?}"
        );
    }

    // ===== D2Config filter_node_ids tests =====

    #[test]
    fn test_d2_filter_node_ids_restricts_output() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();

        // Collect nodes in iteration order so we can pick a specific one.
        let all_nodes: Vec<NodeId> = snapshot.iter_nodes().map(|(id, _)| id).collect();
        assert!(
            all_nodes.len() >= 2,
            "test graph must have at least 2 nodes"
        );

        // Filter to contain only the first node in iteration order.
        let kept_id = all_nodes[0];
        let expected_key = format!("n{}:", kept_id.index());
        // The excluded node key must NOT appear in the output.
        let excluded_key = format!("n{}:", all_nodes[1].index());

        let mut filter = HashSet::new();
        filter.insert(kept_id);

        let config = D2Config::default().with_filter_node_ids(Some(filter));
        let exporter = UnifiedD2Exporter::with_config(&snapshot, config);
        let output = exporter.export();

        // D2 node definitions look like `nN: "label" {`
        let node_defs: Vec<&str> = output
            .lines()
            .filter(|l| {
                let trimmed = l.trim_start();
                (trimmed.starts_with('n') && trimmed.contains(": {"))
                    || (trimmed.starts_with('n') && trimmed.contains(": \""))
            })
            .collect();

        assert_eq!(
            node_defs.len(),
            1,
            "filtered export should have exactly 1 node, got: {node_defs:?}"
        );

        // The single emitted node must be the one we kept.
        assert!(
            node_defs[0].trim_start().starts_with(&expected_key),
            "expected node key '{expected_key}' but got: {}",
            node_defs[0]
        );

        // The excluded node must not appear as a node definition in the output.
        let excluded_present = output
            .lines()
            .any(|l| l.trim_start().starts_with(&excluded_key));
        assert!(
            !excluded_present,
            "excluded node key '{excluded_key}' must not appear in filtered output"
        );

        // With only one visible node, no edges can exist between visible nodes.
        let edge_lines: Vec<&str> = output
            .lines()
            .filter(|l| l.contains("->") || l.contains("<->"))
            .collect();
        assert!(
            edge_lines.is_empty(),
            "no edges should appear when only one node is visible, got: {edge_lines:?}"
        );
    }

    // ===== DotConfig filter_node_ids tests =====

    #[test]
    fn test_dot_filter_node_ids_restricts_output() {
        let graph = create_test_graph_for_export();
        let snapshot = graph.snapshot();

        // Collect nodes in iteration order so we can pick a specific one.
        let all_nodes: Vec<NodeId> = snapshot.iter_nodes().map(|(id, _)| id).collect();
        assert!(
            all_nodes.len() >= 2,
            "test graph must have at least 2 nodes"
        );

        // Filter to contain only the first node in iteration order.
        let kept_id = all_nodes[0];
        let expected_key = format!("\"n{}\"", kept_id.index());
        // The excluded node key must NOT appear in the output.
        let excluded_key = format!("\"n{}\"", all_nodes[1].index());

        let mut filter = HashSet::new();
        filter.insert(kept_id);

        let config = DotConfig::default().with_filter_node_ids(Some(filter));
        let exporter = UnifiedDotExporter::with_config(&snapshot, config);
        let output = exporter.export();

        // DOT node definitions look like `  "nN" [label=...`
        let node_defs: Vec<&str> = output
            .lines()
            .filter(|l| {
                let trimmed = l.trim_start();
                trimmed.starts_with('"') && trimmed.contains("[label=")
            })
            .collect();

        assert_eq!(
            node_defs.len(),
            1,
            "filtered export should have exactly 1 node, got: {node_defs:?}"
        );

        // The single emitted node must be the one we kept.
        assert!(
            node_defs[0].trim_start().starts_with(&expected_key),
            "expected node key '{expected_key}' but got: {}",
            node_defs[0]
        );

        // The excluded node must not appear as a definition in the output.
        let excluded_present = output
            .lines()
            .any(|l| l.trim_start().starts_with(&excluded_key) && l.contains("[label="));
        assert!(
            !excluded_present,
            "excluded node key '{excluded_key}' must not appear in filtered output"
        );

        // With only one visible node, no edges can exist between visible nodes.
        let edge_lines: Vec<&str> = output.lines().filter(|l| l.contains("->")).collect();
        assert!(
            edge_lines.is_empty(),
            "no edges should appear when only one node is visible, got: {edge_lines:?}"
        );
    }

    // ===== DotConfig filter tests =====

    #[test]
    fn test_dot_config_filter_language() {
        let config = DotConfig::default().filter_language(Language::Rust);
        assert!(config.filter_languages.contains(&Language::Rust));
    }

    #[test]
    fn test_dot_config_filter_edge() {
        let config = DotConfig::default().filter_edge(EdgeFilter::Calls);
        assert!(config.filter_edges.contains(&EdgeFilter::Calls));
    }

    #[test]
    fn test_dot_config_with_max_depth() {
        let config = DotConfig::default().with_max_depth(5);
        assert_eq!(config.max_depth, Some(5));
    }
}
