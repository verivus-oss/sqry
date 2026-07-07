//! Archify architecture-diagram exporter.
//!
//! Turns a seeded, depth-limited code subgraph into
//! [Archify](https://github.com/tt-a1i/archify) `architecture` diagram JSON.
//! sqry supplies the code-grounded facts (real symbols, kinds, files,
//! languages, calls, HTTP/DB/queue edges); Archify owns the presentation layer
//! (themed HTML/SVG rendering). This module is the bridge and adds no new graph
//! facts, node kinds, or edge kinds.
//!
//! # What it emits
//!
//! A typed model (Section "Emission model") that mirrors Archify's schema
//! exactly, so invalid diagram types, component types, card colours, or extra
//! fields are unrepresentable. Symbols group into tier-typed `components` and
//! package `boundaries`; retained edges aggregate into `connections`; summary
//! `cards` explain the picture; a deterministic grid `layout` fixes placement
//! with no pixel math.
//!
//! # Independent edge retention (decoupled)
//!
//! [`archify_classify_edge`] is this module's **own** edge-retention policy. It
//! retains the semantically rich cross-service edges (`HttpRequest`, `DbQuery`,
//! `MessageQueue`, ...) that the raw dot / d2 / mermaid / json exporters
//! intentionally drop. It does not touch, widen, or share the MCP export
//! walk's `classify_edge_for_export`, so those existing formats are provably
//! unchanged by this feature.
//!
//! # Determinism
//!
//! Same graph + same seeds + same limits produce byte-identical JSON, including
//! across parallel graph rebuilds. Every ordering is explicit: the seeded walk
//! ([`super::subgraph`]) is deterministic, component ids are assigned in sorted
//! cluster-key order with stable collision suffixing, connections sort by
//! `(from, to)`, grid columns follow stable BFS rank, and elision tie-breaks by
//! `(degree, id)`.
//!
//! # Confidence tiers (fail-safe inference)
//!
//! Component-type inference is tiered. High-confidence types are code-evident
//! (a React component node, an HTTP endpoint, a DB target). Medium/Fallback
//! types are inferred and degrade to `backend` / `external`, are tagged
//! `inferred` with their basis in the sublabel, and are listed in an "Inferred
//! tiers" card. Low-confidence `cloud` / `security` name heuristics are
//! deliberately deferred in v1 (never emitted); such clusters fall back to
//! `backend` / `external`.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use serde::Serialize;

use crate::graph::unified::concurrent::GraphSnapshot;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::node::NodeId;
use crate::graph::unified::node::kind::NodeKind;

use super::subgraph::{EdgeRetention, SeededSubgraph, SeededSubgraphConfig, build_seeded_subgraph};

/// Archify JSON schema contract version this exporter targets.
pub const ARCHIFY_SCHEMA_VERSION: u8 = 1;

/// Default cap on the number of emitted components after grouping.
pub const DEFAULT_MAX_COMPONENTS: usize = 40;

/// Archify grid layout hard column cap (schema: `cols` in `1..=12`).
pub const GRID_COL_CAP: usize = 12;

// ============================================================================
// Errors
// ============================================================================

/// Fail-closed self-validation errors. The exporter returns one of these
/// rather than write a malformed diagram to disk.
#[derive(Debug, thiserror::Error)]
pub enum ArchifyError {
    /// The seeded subgraph produced no components (schema requires `minItems:
    /// 1`), for example an empty or fully language-filtered subgraph.
    #[error("archify export produced no components (empty or filtered subgraph)")]
    NoComponents,

    /// No cards were produced (schema/acceptance requires at least one).
    #[error("archify export produced no summary cards")]
    NoCards,

    /// A component id does not match Archify's `^[a-zA-Z][a-zA-Z0-9_-]*$`.
    #[error("component id {0:?} does not match the Archify id pattern")]
    InvalidComponentId(String),

    /// A connection or boundary referenced an id that is not a defined
    /// component (a dangling cross-reference).
    #[error("dangling id reference {0:?} (not a defined component)")]
    DanglingReference(String),

    /// A boundary wraps zero components (schema requires `minItems: 1`).
    #[error("boundary {0:?} wraps no components")]
    EmptyBoundary(String),

    /// A grid row exceeds the 12-column cap even after elision (invariant
    /// breach; indicates a builder bug).
    #[error("grid row {row} has width {width} exceeding the {cap}-column cap")]
    RowTooWide {
        /// The offending row index.
        row: usize,
        /// The row's component count.
        width: usize,
        /// The column cap.
        cap: usize,
    },

    /// Serialization to JSON failed.
    #[error("archify JSON serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

// ============================================================================
// Typed model (mirrors Archify architecture.schema.json)
// ============================================================================

/// Archify component tier type (the seven schema `componentType` values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentType {
    /// UI / client tier.
    Frontend,
    /// Server / service tier.
    Backend,
    /// Data store tier.
    Database,
    /// Cloud service tier (never inferred in v1).
    Cloud,
    /// Security / auth tier (never inferred in v1).
    Security,
    /// Message bus / queue / streaming tier.
    Messagebus,
    /// External / third-party tier.
    External,
}

/// Archify connection visual variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    /// Standard connection.
    Default,
    /// Emphasised connection (e.g. an HTTP or queue hop).
    Emphasis,
    /// Security-crossing connection (never emitted in v1).
    Security,
    /// Dashed / weaker connection (structural or external).
    Dashed,
}

/// Archify card accent colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CardDot {
    /// Cyan accent.
    Cyan,
    /// Emerald accent.
    Emerald,
    /// Violet accent.
    Violet,
    /// Amber accent.
    Amber,
    /// Rose accent.
    Rose,
    /// Orange accent.
    Orange,
    /// Slate accent.
    Slate,
}

/// Archify boundary kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoundaryKind {
    /// A region grouping (e.g. a package/directory).
    Region,
    /// A security group (never emitted in v1).
    SecurityGroup,
}

/// Top-level Archify architecture document.
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyDocument {
    /// Pins the IR contract (`const: 1`).
    pub schema_version: u8,
    /// Always `"architecture"` for this exporter.
    pub diagram_type: &'static str,
    /// Diagram metadata (title, subtitle).
    pub meta: ArchifyMeta,
    /// Grid layout descriptor.
    pub layout: ArchifyLayout,
    /// Emitted components (`minItems: 1`).
    pub components: Vec<ArchifyComponent>,
    /// Package/region boundaries.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub boundaries: Vec<ArchifyBoundary>,
    /// Aggregated connections.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<ArchifyConnection>,
    /// Summary cards (at least one).
    pub cards: Vec<ArchifyCard>,
}

/// Diagram metadata block.
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyMeta {
    /// Diagram title (non-empty).
    pub title: String,
    /// Optional subtitle.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
}

/// Grid layout descriptor (`mode: "grid"` only).
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyLayout {
    /// Always `"grid"`.
    pub mode: &'static str,
    /// Column count (`1..=12`).
    pub cols: usize,
}

/// A single architecture component.
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyComponent {
    /// Stable id matching `^[a-zA-Z][a-zA-Z0-9_-]*$`.
    pub id: String,
    /// Component tier type.
    #[serde(rename = "type")]
    pub component_type: ComponentType,
    /// Human-readable label (non-empty).
    pub label: String,
    /// Optional sublabel (language, symbol count, inference basis).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sublabel: Option<String>,
    /// Optional tag (`"inferred"` for non-High-confidence tiers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    /// Grid row (tier band).
    pub row: usize,
    /// Grid column (stable within-row rank).
    pub col: usize,
}

/// A package/region boundary wrapping component ids.
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyBoundary {
    /// Boundary kind (`region` in v1).
    pub kind: BoundaryKind,
    /// Boundary label (non-empty).
    pub label: String,
    /// Wrapped component ids (`minItems: 1`).
    pub wraps: Vec<String>,
}

/// An aggregated connection between two components.
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyConnection {
    /// Source component id.
    pub from: String,
    /// Target component id.
    pub to: String,
    /// Optional edge label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Visual variant.
    pub variant: Variant,
}

/// A summary card.
#[derive(Debug, Clone, Serialize)]
pub struct ArchifyCard {
    /// Accent colour.
    pub dot: CardDot,
    /// Card title (non-empty).
    pub title: String,
    /// Card body lines.
    pub items: Vec<String>,
}

// ============================================================================
// Edge classification (this module's own, decoupled policy)
// ============================================================================

/// Archify connection category for a retained edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchifyEdgeClass {
    /// HTTP request to an endpoint.
    Http,
    /// gRPC call.
    Grpc,
    /// Database query / table read / table write.
    Db,
    /// Message-queue publish/subscribe.
    Mq,
    /// WebSocket event.
    Ws,
    /// Foreign-function-interface call.
    Ffi,
    /// Ordinary call.
    Calls,
    /// Import (structural).
    Import,
    /// Export (structural).
    Export,
}

impl ArchifyEdgeClass {
    /// Priority for dominant-class selection and stable tie-breaking (lower =
    /// stronger). Mirrors the traversal edge rank so the picture is coherent.
    const fn priority(self) -> u8 {
        match self {
            Self::Http => 0,
            Self::Grpc => 1,
            Self::Db => 2,
            Self::Mq => 3,
            Self::Ws => 4,
            Self::Ffi => 5,
            Self::Calls => 6,
            Self::Import => 7,
            Self::Export => 8,
        }
    }

    /// Whether an edge of this class contributes a "semantic anchor" signal to
    /// its target (i.e. the target is architecturally interesting on its own).
    const fn is_semantic(self) -> bool {
        matches!(
            self,
            Self::Http | Self::Grpc | Self::Db | Self::Mq | Self::Ws | Self::Ffi
        )
    }

    /// The connection label + variant this class projects.
    const fn label_variant(self) -> (&'static str, Variant) {
        match self {
            Self::Http => ("HTTP", Variant::Emphasis),
            Self::Grpc => ("gRPC", Variant::Dashed),
            Self::Db => ("SQL", Variant::Default),
            Self::Mq => ("queue", Variant::Emphasis),
            Self::Ws => ("ws", Variant::Emphasis),
            Self::Ffi => ("FFI", Variant::Dashed),
            Self::Calls => ("calls", Variant::Default),
            Self::Import => ("imports", Variant::Dashed),
            Self::Export => ("exports", Variant::Dashed),
        }
    }
}

/// The Archify edge-retention policy: independent from the raw exporters.
///
/// Retains calls/imports/exports and the semantic cross-service edges,
/// deciding for each whether the BFS should follow it. Structural exports are
/// retained as connections but not traversed (they would over-expand the
/// frontier without adding architectural signal).
#[must_use]
pub fn archify_classify_edge(kind: &EdgeKind) -> Option<EdgeRetention<ArchifyEdgeClass>> {
    let (class, traverse) = match kind {
        EdgeKind::Calls { .. } => (ArchifyEdgeClass::Calls, true),
        EdgeKind::Imports { .. } => (ArchifyEdgeClass::Import, true),
        EdgeKind::Exports { .. } => (ArchifyEdgeClass::Export, false),
        EdgeKind::HttpRequest { .. } => (ArchifyEdgeClass::Http, true),
        EdgeKind::GrpcCall { .. } => (ArchifyEdgeClass::Grpc, true),
        EdgeKind::DbQuery { .. } | EdgeKind::TableRead { .. } | EdgeKind::TableWrite { .. } => {
            (ArchifyEdgeClass::Db, true)
        }
        EdgeKind::MessageQueue { .. } => (ArchifyEdgeClass::Mq, true),
        EdgeKind::WebSocket { .. } => (ArchifyEdgeClass::Ws, true),
        EdgeKind::FfiCall { .. } => (ArchifyEdgeClass::Ffi, true),
        _ => return None,
    };
    Some(EdgeRetention { class, traverse })
}

// ============================================================================
// Confidence + inference
// ============================================================================

/// Inference confidence for a component's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confidence {
    /// Code-evident (a real component/endpoint/DB target). Not tagged.
    High,
    /// Inferred from a weaker signal (external target). Tagged `inferred`.
    Medium,
    /// Fallback default with no signal. Tagged `inferred`.
    Fallback,
}

impl Confidence {
    const fn is_high(self) -> bool {
        matches!(self, Self::High)
    }
}

// ============================================================================
// Exporter configuration
// ============================================================================

/// Archify exporter configuration.
#[derive(Debug, Clone)]
pub struct ArchifyConfig {
    /// Seed description surfaced in the Overview card (e.g. a symbol name).
    pub seed_label: String,
    /// Diagram title. When empty a default is derived from the seed label.
    pub title: String,
    /// BFS depth used for the subgraph (surfaced in cards; the walk itself is
    /// configured through [`SeededSubgraphConfig`]).
    pub max_depth: usize,
    /// Readability cap on emitted components after grouping.
    pub max_components: usize,
}

impl Default for ArchifyConfig {
    fn default() -> Self {
        Self {
            seed_label: String::new(),
            title: String::new(),
            max_depth: super::subgraph::DEFAULT_MAX_DEPTH,
            max_components: DEFAULT_MAX_COMPONENTS,
        }
    }
}

// ============================================================================
// Build entrypoint
// ============================================================================

/// Build an Archify architecture document from concrete seeds.
///
/// Runs the shared seeded-subgraph walk with the Archify edge classifier, then
/// groups, infers, lays out, and self-validates. Fail-closed: returns an
/// [`ArchifyError`] rather than emit a malformed diagram.
///
/// # Errors
///
/// Returns [`ArchifyError`] if the subgraph is empty/fully filtered, or if the
/// self-validation invariants are violated (which would indicate a builder
/// bug).
pub fn build_archify_document(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    subgraph_config: &SeededSubgraphConfig,
    config: &ArchifyConfig,
) -> Result<ArchifyDocument, ArchifyError> {
    Ok(build_archify_document_with_meta(snapshot, seeds, subgraph_config, config)?.document)
}

/// An Archify document plus the seeded-walk signals ([`ArchifyBuildMeta`])
/// callers need to report accurate `truncated` / `total` fields, instead of
/// hardcoding them (the walk itself is the only place that knows whether
/// `max_results` was hit).
#[derive(Debug, Clone)]
pub struct ArchifyBuildMeta {
    /// The built Archify architecture document.
    pub document: ArchifyDocument,
    /// `true` if the seeded subgraph walk stopped early because
    /// `subgraph_config.max_results` was reached (see
    /// [`super::subgraph::SeededSubgraph::truncated`]).
    pub truncated: bool,
    /// Number of retained edges the seeded walk produced (before elision /
    /// grid placement pruned components, but after the Archify edge
    /// classifier's own retention policy).
    pub total_edges: usize,
}

/// Build an Archify document, also returning the seeded-walk's
/// truncation/total signals.
///
/// # Errors
///
/// Returns [`ArchifyError`] if the subgraph is empty/fully filtered, or if the
/// self-validation invariants are violated (which would indicate a builder
/// bug).
pub fn build_archify_document_with_meta(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    subgraph_config: &SeededSubgraphConfig,
    config: &ArchifyConfig,
) -> Result<ArchifyBuildMeta, ArchifyError> {
    let subgraph = build_seeded_subgraph(snapshot, seeds, subgraph_config, archify_classify_edge);
    let truncated = subgraph.truncated;
    let total_edges = subgraph.edges.len();
    let document = ArchifyBuilder {
        snapshot,
        subgraph: &subgraph,
        config,
    }
    .build()?;
    Ok(ArchifyBuildMeta {
        document,
        truncated,
        total_edges,
    })
}

/// Build an Archify document and serialize it to pretty JSON bytes-string.
///
/// # Errors
///
/// Propagates [`ArchifyError`] from the build/self-validation, or a
/// serialization error.
pub fn export_archify_json(
    snapshot: &GraphSnapshot,
    seeds: &[NodeId],
    subgraph_config: &SeededSubgraphConfig,
    config: &ArchifyConfig,
) -> Result<String, ArchifyError> {
    let doc = build_archify_document(snapshot, seeds, subgraph_config, config)?;
    Ok(serde_json::to_string_pretty(&doc)?)
}

// ============================================================================
// Builder
// ============================================================================

/// A component under construction (pre-serialization).
struct DraftComponent {
    /// Stable cluster key (used for id assignment ordering).
    cluster_key: String,
    /// Assigned Archify id (filled during id assignment).
    id: String,
    label: String,
    package: String,
    language: String,
    node_count: usize,
    component_type: ComponentType,
    confidence: Confidence,
    basis: String,
    /// Earliest BFS discovery rank across the cluster's nodes (for col order).
    bfs_rank: usize,
    /// Assigned grid `(row, col)` (filled during grid placement).
    grid: (usize, usize),
}

struct ArchifyBuilder<'a> {
    snapshot: &'a GraphSnapshot,
    subgraph: &'a SeededSubgraph<ArchifyEdgeClass>,
    config: &'a ArchifyConfig,
}

impl ArchifyBuilder<'_> {
    fn build(&self) -> Result<ArchifyDocument, ArchifyError> {
        if self.subgraph.nodes.is_empty() {
            return Err(ArchifyError::NoComponents);
        }

        // 1. Per-node signals from retained edges.
        let semantic_targets = self.semantic_target_set();
        let (node_to_cluster, mut drafts) = self.cluster_nodes(&semantic_targets);

        if drafts.is_empty() {
            return Err(ArchifyError::NoComponents);
        }

        // 2. Type inference per cluster.
        self.infer_types(&node_to_cluster, &mut drafts);

        // 3. Assign stable ids (sorted cluster-key order, collision suffixing).
        assign_ids(&mut drafts);

        // 4. Aggregate connections between clusters.
        let mut connections = self.aggregate_connections(&node_to_cluster, &drafts);

        // 5. Elide down to max_components + per-row width cap.
        let low_degree_elided =
            elide_components(&mut drafts, &connections, self.config.max_components);

        // 6. Grid placement (rows by tier, cols by BFS rank). `place_grid` can
        // drop further components when a row exceeds `GRID_COL_CAP`; that
        // count must be folded into the caveat total below, otherwise the
        // caveat card under-reports how many components actually vanished.
        let (cols, grid_cap_elided) = place_grid(&mut drafts)?;
        let elided = low_degree_elided + grid_cap_elided;

        // 7. Drop connections that reference elided components.
        let live_ids: HashSet<&str> = drafts.iter().map(|d| d.id.as_str()).collect();
        connections
            .retain(|c| live_ids.contains(c.from.as_str()) && live_ids.contains(c.to.as_str()));

        // 8. Boundaries per package.
        let boundaries = build_boundaries(&drafts);

        // 9. Cards (Overview always present).
        let languages = self.languages_present();
        let cards = build_cards(&drafts, &connections, self.config, &languages, elided);

        // 10. Assemble + self-validate (fail-closed).
        let components: Vec<ArchifyComponent> = drafts.iter().map(DraftComponent::finish).collect();

        let title = if self.config.title.trim().is_empty() {
            if self.config.seed_label.trim().is_empty() {
                "Architecture".to_string()
            } else {
                format!("Architecture: {}", self.config.seed_label)
            }
        } else {
            self.config.title.clone()
        };

        let doc = ArchifyDocument {
            schema_version: ARCHIFY_SCHEMA_VERSION,
            diagram_type: "architecture",
            meta: ArchifyMeta {
                title,
                subtitle: None,
            },
            layout: ArchifyLayout { mode: "grid", cols },
            components,
            boundaries,
            connections,
            cards,
        };

        self_validate(&doc)?;
        Ok(doc)
    }

    /// Set of node ids that are the target of at least one semantic edge.
    fn semantic_target_set(&self) -> HashSet<NodeId> {
        self.subgraph
            .edges
            .iter()
            .filter(|e| e.class.is_semantic())
            .map(|e| e.to)
            .collect()
    }

    /// Assign every visited node to a cluster and produce draft components.
    ///
    /// Standalone anchors (semantic node kinds or semantic-edge targets) get
    /// their own cluster; everything else groups by file path.
    fn cluster_nodes(
        &self,
        semantic_targets: &HashSet<NodeId>,
    ) -> (HashMap<NodeId, usize>, Vec<DraftComponent>) {
        let mut node_to_cluster: HashMap<NodeId, usize> = HashMap::new();
        let mut key_to_index: HashMap<String, usize> = HashMap::new();
        let mut drafts: Vec<DraftComponent> = Vec::new();

        for (rank, &node) in self.subgraph.nodes.iter().enumerate() {
            let Some(entry) = self.snapshot.get_node(node) else {
                continue;
            };
            let is_anchor = is_anchor_kind(entry.kind) || semantic_targets.contains(&node);
            let (cluster_key, label) = if is_anchor {
                (
                    format!("anchor:{}", node.index()),
                    self.node_short_name(node),
                )
            } else {
                let file = self.node_file(node);
                (format!("file:{file}"), file_basename(&file))
            };

            let idx = *key_to_index.entry(cluster_key.clone()).or_insert_with(|| {
                let file = self.node_file(node);
                drafts.push(DraftComponent {
                    cluster_key: cluster_key.clone(),
                    id: String::new(),
                    label,
                    package: package_of(&file),
                    language: self.node_language(node),
                    node_count: 0,
                    component_type: ComponentType::Backend,
                    confidence: Confidence::Fallback,
                    basis: "internal callable cluster (no other signal)".to_string(),
                    bfs_rank: rank,
                    grid: (0, 0),
                });
                drafts.len() - 1
            });

            let draft = &mut drafts[idx];
            draft.node_count += 1;
            draft.bfs_rank = draft.bfs_rank.min(rank);
            node_to_cluster.insert(node, idx);
        }

        (node_to_cluster, drafts)
    }

    /// Infer a component type + confidence for each cluster.
    fn infer_types(&self, node_to_cluster: &HashMap<NodeId, usize>, drafts: &mut [DraftComponent]) {
        // Per-cluster signal accumulation.
        let n = drafts.len();
        let mut has_frontend = vec![false; n];
        let mut has_endpoint = vec![false; n];
        let mut is_http_target = vec![false; n];
        let mut is_db_target = vec![false; n];
        let mut is_mq_target = vec![false; n];
        let mut is_external_target = vec![false; n];

        // Node-kind signals.
        for (&node, &idx) in node_to_cluster {
            if let Some(entry) = self.snapshot.get_node(node) {
                match entry.kind {
                    NodeKind::Component => has_frontend[idx] = true,
                    NodeKind::Endpoint => has_endpoint[idx] = true,
                    _ => {}
                }
            }
        }

        // Edge-target signals.
        for edge in &self.subgraph.edges {
            let Some(&idx) = node_to_cluster.get(&edge.to) else {
                continue;
            };
            match edge.class {
                ArchifyEdgeClass::Http => is_http_target[idx] = true,
                ArchifyEdgeClass::Db => is_db_target[idx] = true,
                ArchifyEdgeClass::Mq | ArchifyEdgeClass::Ws => is_mq_target[idx] = true,
                ArchifyEdgeClass::Grpc | ArchifyEdgeClass::Ffi => is_external_target[idx] = true,
                _ => {}
            }
        }

        for (idx, draft) in drafts.iter_mut().enumerate() {
            // Precedence: highest-confidence, most-specific signal wins.
            let (ty, conf, basis) = if has_frontend[idx] {
                (
                    ComponentType::Frontend,
                    Confidence::High,
                    "UI component node".to_string(),
                )
            } else if has_endpoint[idx] || is_http_target[idx] {
                (
                    ComponentType::Backend,
                    Confidence::High,
                    if has_endpoint[idx] {
                        "HTTP endpoint node".to_string()
                    } else {
                        "target of HTTP request edges".to_string()
                    },
                )
            } else if is_db_target[idx] {
                (
                    ComponentType::Database,
                    Confidence::High,
                    "target of database query edges".to_string(),
                )
            } else if is_mq_target[idx] {
                (
                    ComponentType::Messagebus,
                    Confidence::High,
                    "target of message-queue / websocket edges".to_string(),
                )
            } else if is_external_target[idx] {
                (
                    ComponentType::External,
                    Confidence::Medium,
                    "target of gRPC / FFI edges".to_string(),
                )
            } else {
                (
                    ComponentType::Backend,
                    Confidence::Fallback,
                    "internal callable cluster (no other signal)".to_string(),
                )
            };
            draft.component_type = ty;
            draft.confidence = conf;
            draft.basis = basis;
        }
    }

    /// Aggregate retained cross-cluster edges into deduped connections.
    fn aggregate_connections(
        &self,
        node_to_cluster: &HashMap<NodeId, usize>,
        drafts: &[DraftComponent],
    ) -> Vec<ArchifyConnection> {
        // (from_idx, to_idx) -> class -> count.
        let mut agg: BTreeMap<(usize, usize), HashMap<ArchifyEdgeClass, usize>> = BTreeMap::new();
        for edge in &self.subgraph.edges {
            let (Some(&from_idx), Some(&to_idx)) = (
                node_to_cluster.get(&edge.from),
                node_to_cluster.get(&edge.to),
            ) else {
                continue;
            };
            if from_idx == to_idx {
                continue; // intra-cluster edges collapse into the component
            }
            *agg.entry((from_idx, to_idx))
                .or_default()
                .entry(edge.class)
                .or_insert(0) += 1;
        }

        let mut connections: Vec<ArchifyConnection> = agg
            .into_iter()
            .map(|((from_idx, to_idx), classes)| {
                let dominant = dominant_class(&classes);
                let (label, variant) = dominant.label_variant();
                ArchifyConnection {
                    from: drafts[from_idx].id.clone(),
                    to: drafts[to_idx].id.clone(),
                    label: Some(label.to_string()),
                    variant,
                }
            })
            .collect();

        // Deterministic order by (from, to).
        connections.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));
        connections
    }

    /// Sorted, de-duplicated list of languages present in the subgraph.
    fn languages_present(&self) -> Vec<String> {
        let mut langs: HashSet<String> = HashSet::new();
        for &node in &self.subgraph.nodes {
            langs.insert(self.node_language(node));
        }
        let mut langs: Vec<String> = langs.into_iter().collect();
        langs.sort();
        langs
    }

    // ---- node field resolution helpers ----

    fn node_short_name(&self, node: NodeId) -> String {
        self.snapshot
            .get_node(node)
            .and_then(|e| self.snapshot.strings().resolve(e.name))
            .map_or_else(|| format!("node{}", node.index()), |s| s.to_string())
    }

    fn node_file(&self, node: NodeId) -> String {
        self.snapshot
            .get_node(node)
            .and_then(|e| self.snapshot.files().resolve(e.file))
            .map_or_else(String::new, |p| p.to_string_lossy().replace('\\', "/"))
    }

    fn node_language(&self, node: NodeId) -> String {
        self.snapshot
            .get_node(node)
            .and_then(|e| self.snapshot.files().language_for_file(e.file))
            .map_or_else(|| "unknown".to_string(), |l| l.to_string())
    }
}

impl DraftComponent {
    fn finish(&self) -> ArchifyComponent {
        let mut sublabel = format!("{} · {} sym", self.language, self.node_count);
        let tag = if self.confidence.is_high() {
            None
        } else {
            // Surface the inference basis on the sublabel for non-High tiers.
            sublabel = format!("{sublabel} · inferred: {}", self.basis);
            Some("inferred".to_string())
        };
        ArchifyComponent {
            id: self.id.clone(),
            component_type: self.component_type,
            label: self.label.clone(),
            sublabel: Some(sublabel),
            tag,
            row: self.grid.0,
            col: self.grid.1,
        }
    }
}

// ============================================================================
// Free helpers
// ============================================================================

const fn is_anchor_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Endpoint
            | NodeKind::Service
            | NodeKind::Resource
            | NodeKind::Channel
            | NodeKind::Component
    )
}

/// Pick the dominant class: highest count, ties broken by class priority.
fn dominant_class(classes: &HashMap<ArchifyEdgeClass, usize>) -> ArchifyEdgeClass {
    classes
        .iter()
        .max_by(|a, b| {
            a.1.cmp(b.1)
                .then_with(|| b.0.priority().cmp(&a.0.priority()))
        })
        .map_or(ArchifyEdgeClass::Calls, |(&class, _)| class)
}

/// Immediate parent directory of a workspace-relative file path (the package),
/// or `(root)` for a top-level file.
fn package_of(file: &str) -> String {
    match file.rsplit_once('/') {
        Some((dir, _)) if !dir.is_empty() => dir.to_string(),
        _ => "(root)".to_string(),
    }
}

/// File basename for a component label.
fn file_basename(file: &str) -> String {
    file.rsplit_once('/')
        .map_or_else(|| file.to_string(), |(_, base)| base.to_string())
}

/// Sanitize any string into Archify's `^[a-zA-Z][a-zA-Z0-9_-]*$` id pattern.
///
/// Deterministic: disallowed characters become `-`, consecutive `-` collapse,
/// and an id not starting with a letter is prefixed with `c`. Never empty.
fn sanitize_id(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 1);
    let mut last_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let mut result = trimmed.to_string();
    if result.is_empty()
        || !result
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
    {
        result = format!("c{result}");
    }
    result
}

/// Assign stable ids to drafts in sorted cluster-key order, suffixing on
/// collision so cross-references stay valid.
fn assign_ids(drafts: &mut [DraftComponent]) {
    let mut order: Vec<usize> = (0..drafts.len()).collect();
    order.sort_by(|&a, &b| drafts[a].cluster_key.cmp(&drafts[b].cluster_key));

    let mut seen: HashMap<String, u32> = HashMap::new();
    for &i in &order {
        let base = sanitize_id(&drafts[i].label);
        let id = match seen.get_mut(&base) {
            Some(count) => {
                *count += 1;
                format!("{base}-{count}")
            }
            None => {
                seen.insert(base.clone(), 0);
                base
            }
        };
        drafts[i].id = id;
    }
}

/// Elide low-degree components to satisfy `max_components`. Returns the number
/// elided. Tie-break: lowest degree, then id ascending (stable).
fn elide_components(
    drafts: &mut Vec<DraftComponent>,
    connections: &[ArchifyConnection],
    max_components: usize,
) -> usize {
    if drafts.len() <= max_components {
        return 0;
    }
    let degree = component_degrees(drafts, connections);

    // Rank components by keep-priority: higher degree kept; ties by id asc.
    let mut order: Vec<usize> = (0..drafts.len()).collect();
    order.sort_by(|&a, &b| {
        degree[b]
            .cmp(&degree[a])
            .then_with(|| drafts[a].id.cmp(&drafts[b].id))
    });
    let keep: HashSet<usize> = order.into_iter().take(max_components).collect();

    let elided = drafts.len() - keep.len();
    let mut idx = 0;
    drafts.retain(|_| {
        let keep_this = keep.contains(&idx);
        idx += 1;
        keep_this
    });
    elided
}

/// Degree (connection count touching each component) by draft index.
fn component_degrees(drafts: &[DraftComponent], connections: &[ArchifyConnection]) -> Vec<usize> {
    let id_to_idx: HashMap<&str, usize> = drafts
        .iter()
        .enumerate()
        .map(|(i, d)| (d.id.as_str(), i))
        .collect();
    let mut degree = vec![0usize; drafts.len()];
    for c in connections {
        if let Some(&i) = id_to_idx.get(c.from.as_str()) {
            degree[i] += 1;
        }
        if let Some(&i) = id_to_idx.get(c.to.as_str()) {
            degree[i] += 1;
        }
    }
    degree
}

/// Assign grid rows (by tier) and columns (by BFS rank within row), enforcing
/// the 12-column cap by further elision inside any over-wide row.
///
/// Returns `(cols, dropped)`: the chosen `cols` value and the total number of
/// components this call (including any recursive re-placement passes) had to
/// drop to satisfy the per-row cap. Callers must fold `dropped` into the
/// overall elided-component count fed to the caveat card, since these drops
/// happen after (and independently of) `elide_components`.
fn place_grid(drafts: &mut Vec<DraftComponent>) -> Result<(usize, usize), ArchifyError> {
    // Compute component set that survives the per-row width cap. Group indices
    // by row.
    let mut rows: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, d) in drafts.iter().enumerate() {
        rows.entry(row_for_type(d.component_type))
            .or_default()
            .push(i);
    }

    // Within each row, order by BFS rank (then id) and cap width; over-cap
    // components are elided.
    let mut drop: HashSet<usize> = HashSet::new();
    let mut placement: HashMap<usize, (usize, usize)> = HashMap::new();
    for (&row, indices) in &rows {
        let mut ordered = indices.clone();
        ordered.sort_by(|&a, &b| {
            drafts[a]
                .bfs_rank
                .cmp(&drafts[b].bfs_rank)
                .then_with(|| drafts[a].id.cmp(&drafts[b].id))
        });
        for (col, &i) in ordered.iter().enumerate() {
            if col >= GRID_COL_CAP {
                drop.insert(i);
            } else {
                placement.insert(i, (row, col));
            }
        }
    }

    if !drop.is_empty() {
        let dropped_here = drop.len();
        let mut idx = 0;
        drafts.retain(|_| {
            let keep = !drop.contains(&idx);
            idx += 1;
            keep
        });
        // Re-place after dropping (indices shifted); accumulate the drop
        // count across recursive passes so the caller sees the true total.
        let (cols, dropped_rest) = place_grid(drafts)?;
        return Ok((cols, dropped_here + dropped_rest));
    }

    let mut max_width = 1usize;
    for (i, d) in drafts.iter_mut().enumerate() {
        let (row, col) = placement.get(&i).copied().unwrap_or((0, 0));
        d.set_grid(row, col);
        max_width = max_width.max(col + 1);
    }

    // Invariant: no row wider than the cap.
    let mut widths: BTreeMap<usize, usize> = BTreeMap::new();
    for d in drafts.iter() {
        *widths.entry(d.grid_row()).or_insert(0) += 1;
    }
    for (&row, &width) in &widths {
        if width > GRID_COL_CAP {
            return Err(ArchifyError::RowTooWide {
                row,
                width,
                cap: GRID_COL_CAP,
            });
        }
    }

    Ok((max_width.clamp(1, GRID_COL_CAP), 0))
}

/// Fixed tier -> grid row mapping (bands top to bottom).
const fn row_for_type(ty: ComponentType) -> usize {
    match ty {
        ComponentType::Frontend => 0,
        ComponentType::External | ComponentType::Security => 1,
        ComponentType::Backend => 2,
        ComponentType::Messagebus => 3,
        ComponentType::Database => 4,
        ComponentType::Cloud => 5,
    }
}

/// Build one region boundary per package (sorted), wrapping its component ids.
fn build_boundaries(drafts: &[DraftComponent]) -> Vec<ArchifyBoundary> {
    let mut by_package: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for d in drafts {
        by_package
            .entry(d.package.clone())
            .or_default()
            .push(d.id.clone());
    }
    by_package
        .into_iter()
        .map(|(label, mut wraps)| {
            wraps.sort();
            ArchifyBoundary {
                kind: BoundaryKind::Region,
                label,
                wraps,
            }
        })
        .collect()
}

/// Build the summary cards. The Overview card is always present.
fn build_cards(
    drafts: &[DraftComponent],
    connections: &[ArchifyConnection],
    config: &ArchifyConfig,
    languages: &[String],
    elided: usize,
) -> Vec<ArchifyCard> {
    let mut cards = Vec::new();

    let seed = if config.seed_label.trim().is_empty() {
        "(unspecified)".to_string()
    } else {
        config.seed_label.clone()
    };
    cards.push(ArchifyCard {
        dot: CardDot::Cyan,
        title: "Overview".to_string(),
        items: vec![
            format!("Seed: {seed}"),
            format!("Depth: {}", config.max_depth),
            format!("Components: {}", drafts.len()),
            format!("Connections: {}", connections.len()),
            format!("Languages: {}", languages.join(", ")),
        ],
    });

    // Inferred tiers (only if any non-High component exists).
    let inferred: Vec<String> = drafts
        .iter()
        .filter(|d| !d.confidence.is_high())
        .map(|d| {
            format!(
                "{}: {} inferred from {}",
                d.label,
                component_type_str(d.component_type),
                d.basis
            )
        })
        .collect();
    if !inferred.is_empty() {
        cards.push(ArchifyCard {
            dot: CardDot::Violet,
            title: "Inferred tiers".to_string(),
            items: inferred,
        });
    }

    // Caveats (always present).
    let mut caveats = vec![
        "Seeded subgraph, not the whole repository.".to_string(),
        format!(
            "Limits: max_depth={}, max_components={}.",
            config.max_depth, config.max_components
        ),
    ];
    if elided > 0 {
        caveats.push(format!(
            "Elided {elided} component(s) for readability (low-degree pruning and/or grid-width limits)."
        ));
    }
    caveats.push(
        "sqry supplies code-grounded facts; Archify handles layout and presentation.".to_string(),
    );
    cards.push(ArchifyCard {
        dot: CardDot::Amber,
        title: "Caveats".to_string(),
        items: caveats,
    });

    cards
}

const fn component_type_str(ty: ComponentType) -> &'static str {
    match ty {
        ComponentType::Frontend => "frontend",
        ComponentType::Backend => "backend",
        ComponentType::Database => "database",
        ComponentType::Cloud => "cloud",
        ComponentType::Security => "security",
        ComponentType::Messagebus => "messagebus",
        ComponentType::External => "external",
    }
}

/// Regex-free check for the Archify id pattern `^[a-zA-Z][a-zA-Z0-9_-]*$`.
fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Fail-closed self-validation of the assembled document.
fn self_validate(doc: &ArchifyDocument) -> Result<(), ArchifyError> {
    if doc.components.is_empty() {
        return Err(ArchifyError::NoComponents);
    }
    if doc.cards.is_empty() {
        return Err(ArchifyError::NoCards);
    }

    let ids: HashSet<&str> = doc.components.iter().map(|c| c.id.as_str()).collect();

    for c in &doc.components {
        if !is_valid_id(&c.id) {
            return Err(ArchifyError::InvalidComponentId(c.id.clone()));
        }
    }
    for conn in &doc.connections {
        if !ids.contains(conn.from.as_str()) {
            return Err(ArchifyError::DanglingReference(conn.from.clone()));
        }
        if !ids.contains(conn.to.as_str()) {
            return Err(ArchifyError::DanglingReference(conn.to.clone()));
        }
    }
    for b in &doc.boundaries {
        if b.wraps.is_empty() {
            return Err(ArchifyError::EmptyBoundary(b.label.clone()));
        }
        for w in &b.wraps {
            if !ids.contains(w.as_str()) {
                return Err(ArchifyError::DanglingReference(w.clone()));
            }
        }
    }

    // Grid row-width invariant.
    let mut widths: BTreeMap<usize, usize> = BTreeMap::new();
    for c in &doc.components {
        *widths.entry(c.row).or_insert(0) += 1;
    }
    for (&row, &width) in &widths {
        if width > GRID_COL_CAP {
            return Err(ArchifyError::RowTooWide {
                row,
                width,
                cap: GRID_COL_CAP,
            });
        }
    }

    Ok(())
}

impl DraftComponent {
    fn set_grid(&mut self, row: usize, col: usize) {
        self.grid = (row, col);
    }
    const fn grid_row(&self) -> usize {
        self.grid.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::node::Language;
    use crate::graph::unified::concurrent::CodeGraph;
    use crate::graph::unified::edge::{
        BidirectionalEdgeStore, DbQueryType, EdgeKind, HttpMethod, ResolvedVia,
    };
    use crate::graph::unified::node::NodeId;
    use crate::graph::unified::storage::NodeEntry;
    use crate::graph::unified::storage::arena::NodeArena;
    use crate::graph::unified::storage::indices::AuxiliaryIndices;
    use crate::graph::unified::storage::interner::StringInterner;
    use crate::graph::unified::storage::registry::FileRegistry;
    use std::path::Path;

    /// Minimal node builder for fixtures.
    #[allow(clippy::too_many_arguments)]
    fn node(
        arena: &mut NodeArena,
        strings: &mut StringInterner,
        kind: NodeKind,
        name: &str,
        qname: &str,
        file: crate::graph::unified::FileId,
        start_byte: u32,
    ) -> NodeId {
        let name_id = strings.intern(name).unwrap();
        let qname_id = strings.intern(qname).unwrap();
        arena
            .alloc(NodeEntry {
                kind,
                name: name_id,
                file,
                start_byte,
                end_byte: start_byte + 50,
                start_line: 1,
                start_column: 0,
                end_line: 5,
                end_column: 1,
                signature: None,
                doc: None,
                qualified_name: Some(qname_id),
                visibility: None,
                is_async: false,
                is_static: false,
                is_unsafe: false,
                is_definition: true,
                body_hash: None,
            })
            .unwrap()
    }

    struct Fixture {
        graph: CodeGraph,
        dashboard: NodeId,
    }

    /// A small polyglot architecture: frontend component -> HTTP endpoint ->
    /// handler -> query fn -> database table.
    fn build_fixture() -> Fixture {
        let mut arena = NodeArena::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let edges = BidirectionalEdgeStore::new();
        let indices = AuxiliaryIndices::new();

        let f_web = files
            .register_with_language(Path::new("web/app.jsx"), Some(Language::JavaScript))
            .unwrap();
        let f_routes = files
            .register_with_language(Path::new("api/routes.rs"), Some(Language::Rust))
            .unwrap();
        let f_handlers = files
            .register_with_language(Path::new("api/handlers.rs"), Some(Language::Rust))
            .unwrap();
        let f_queries = files
            .register_with_language(Path::new("db/queries.rs"), Some(Language::Rust))
            .unwrap();
        let f_schema = files
            .register_with_language(Path::new("db/schema.sql"), Some(Language::Sql))
            .unwrap();

        let dashboard = node(
            &mut arena,
            &mut strings,
            NodeKind::Component,
            "Dashboard",
            "web::Dashboard",
            f_web,
            0,
        );
        let endpoint = node(
            &mut arena,
            &mut strings,
            NodeKind::Endpoint,
            "users_endpoint",
            "api::users_endpoint",
            f_routes,
            100,
        );
        let handler = node(
            &mut arena,
            &mut strings,
            NodeKind::Function,
            "handle_users",
            "api::handle_users",
            f_handlers,
            200,
        );
        let query_fn = node(
            &mut arena,
            &mut strings,
            NodeKind::Function,
            "run_query",
            "db::run_query",
            f_queries,
            300,
        );
        let table = node(
            &mut arena,
            &mut strings,
            NodeKind::Resource,
            "users",
            "db::users",
            f_schema,
            400,
        );

        let url = strings.intern("/users").unwrap();
        let tbl = strings.intern("users").unwrap();

        edges.add_edge(
            dashboard,
            endpoint,
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: Some(url),
            },
            f_web,
        );
        edges.add_edge(
            endpoint,
            handler,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            f_routes,
        );
        edges.add_edge(
            handler,
            query_fn,
            EdgeKind::Calls {
                argument_count: 1,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            f_handlers,
        );
        edges.add_edge(
            query_fn,
            table,
            EdgeKind::DbQuery {
                query_type: DbQueryType::Select,
                table: Some(tbl),
            },
            f_queries,
        );

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );
        Fixture { graph, dashboard }
    }

    fn full_config() -> (SeededSubgraphConfig, ArchifyConfig) {
        let sub = SeededSubgraphConfig {
            max_depth: 5,
            max_results: 1000,
            languages: Vec::new(),
        }
        .normalized();
        let arch = ArchifyConfig {
            seed_label: "web::Dashboard".to_string(),
            title: String::new(),
            max_depth: sub.max_depth,
            max_components: DEFAULT_MAX_COMPONENTS,
        };
        (sub, arch)
    }

    // ---- sanitize_id ----

    #[test]
    fn sanitize_id_matches_pattern() {
        for raw in [
            "crate::mod::Fn",
            "/api/users",
            "123file",
            "src/main.rs",
            "GET /users",
            "café::naïve",
            "",
            "---",
            "_leading",
            "a",
        ] {
            let id = sanitize_id(raw);
            assert!(
                is_valid_id(&id),
                "id {id:?} from {raw:?} is not schema-valid"
            );
        }
    }

    #[test]
    fn sanitize_id_is_deterministic() {
        assert_eq!(sanitize_id("crate::mod::Fn"), sanitize_id("crate::mod::Fn"));
        assert_eq!(sanitize_id("a::b"), "a-b");
        assert_eq!(sanitize_id("123"), "c123");
    }

    #[test]
    fn assign_ids_suffixes_collisions() {
        let mut drafts = vec![
            draft("file:a/x.rs", "x.rs"),
            draft("file:b/x.rs", "x.rs"),
            draft("file:c/x.rs", "x.rs"),
        ];
        assign_ids(&mut drafts);
        let ids: Vec<&str> = drafts.iter().map(|d| d.id.as_str()).collect();
        // Sorted cluster-key order -> a, b, c get x-rs, x-rs-1, x-rs-2.
        assert_eq!(ids, vec!["x-rs", "x-rs-1", "x-rs-2"]);
    }

    fn draft(cluster_key: &str, label: &str) -> DraftComponent {
        DraftComponent {
            cluster_key: cluster_key.to_string(),
            id: String::new(),
            label: label.to_string(),
            package: "pkg".to_string(),
            language: "rust".to_string(),
            node_count: 1,
            component_type: ComponentType::Backend,
            confidence: Confidence::High,
            basis: String::new(),
            bfs_rank: 0,
            grid: (0, 0),
        }
    }

    // ---- inference + structure ----

    #[test]
    fn exports_expected_tiers_and_structure() {
        let fx = build_fixture();
        let (sub, arch) = full_config();
        let doc = build_archify_document(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch)
            .expect("archify doc");

        // Frontend component is code-evident, not tagged inferred.
        let dashboard = doc
            .components
            .iter()
            .find(|c| c.label == "Dashboard")
            .expect("dashboard component");
        assert_eq!(dashboard.component_type, ComponentType::Frontend);
        assert!(dashboard.tag.is_none());

        // Endpoint node -> backend High.
        let endpoint = doc
            .components
            .iter()
            .find(|c| c.label == "users_endpoint")
            .expect("endpoint component");
        assert_eq!(endpoint.component_type, ComponentType::Backend);

        // DB target -> database High.
        let db = doc
            .components
            .iter()
            .find(|c| c.label == "users")
            .expect("db component");
        assert_eq!(db.component_type, ComponentType::Database);

        // At least one card (Overview), and the standing sqry/Archify note.
        assert!(!doc.cards.is_empty());
        assert!(doc.cards.iter().any(|c| c.title == "Overview"));
        assert!(
            doc.cards
                .iter()
                .flat_map(|c| &c.items)
                .any(|i| i.contains("Archify handles layout"))
        );

        // HTTP connection is emphasised.
        assert!(
            doc.connections
                .iter()
                .any(|c| matches!(c.variant, Variant::Emphasis)
                    && c.label.as_deref() == Some("HTTP"))
        );

        // Boundaries wrap real component ids.
        assert!(!doc.boundaries.is_empty());
        let ids: HashSet<&str> = doc.components.iter().map(|c| c.id.as_str()).collect();
        for b in &doc.boundaries {
            assert!(!b.wraps.is_empty());
            for w in &b.wraps {
                assert!(ids.contains(w.as_str()));
            }
        }
    }

    #[test]
    fn no_cloud_or_security_inference_in_v1() {
        // Names that a heuristic might tag cloud/security must stay fallback.
        let mut arena = NodeArena::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let edges = BidirectionalEdgeStore::new();
        let indices = AuxiliaryIndices::new();
        let f = files
            .register_with_language(Path::new("svc/auth.rs"), Some(Language::Rust))
            .unwrap();
        let jwt = node(
            &mut arena,
            &mut strings,
            NodeKind::Function,
            "JwtAuthClient",
            "svc::JwtAuthClient",
            f,
            0,
        );
        let s3 = node(
            &mut arena,
            &mut strings,
            NodeKind::Function,
            "S3Bucket",
            "svc::S3Bucket",
            f,
            100,
        );
        edges.add_edge(
            jwt,
            s3,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            f,
        );
        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );
        let (sub, mut arch) = full_config();
        arch.seed_label = "svc::JwtAuthClient".to_string();
        let doc = build_archify_document(&graph.snapshot(), &[jwt], &sub, &arch).unwrap();
        for c in &doc.components {
            assert_ne!(c.component_type, ComponentType::Cloud);
            assert_ne!(c.component_type, ComponentType::Security);
        }
    }

    // ---- determinism ----

    #[test]
    fn output_is_byte_identical_across_repeats() {
        let fx = build_fixture();
        let (sub, arch) = full_config();
        let a = export_archify_json(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch).unwrap();
        let b = export_archify_json(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn output_is_stable_across_parallel_rebuilds() {
        // Rebuild the fixture graph independently several times (arena/edge
        // allocation order can differ) and assert identical bytes. The build
        // here is single-threaded, but the exporter's determinism must not
        // depend on iteration order, which is what this locks.
        let (sub, arch) = full_config();
        let outputs: Vec<String> = (0..8)
            .map(|_| {
                let fx = build_fixture();
                export_archify_json(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch).unwrap()
            })
            .collect();
        for o in &outputs[1..] {
            assert_eq!(&outputs[0], o);
        }
    }

    // ---- limits / elision ----

    #[test]
    fn respects_max_components_and_row_width() {
        let fx = build_fixture();
        let (sub, mut arch) = full_config();
        arch.max_components = 2;
        let doc = build_archify_document(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch)
            .expect("archify doc");
        assert!(doc.components.len() <= 2, "elided to max_components");
        // Row-width invariant.
        let mut widths: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for c in &doc.components {
            *widths.entry(c.row).or_insert(0) += 1;
        }
        assert!(widths.values().all(|&w| w <= GRID_COL_CAP));
        // Caveat card reflects elision.
        assert!(
            doc.cards
                .iter()
                .flat_map(|c| &c.items)
                .any(|i| i.contains("Elided"))
        );
    }

    /// Regression test for the caveat under-reporting elided components: a
    /// grid-cap drop inside `place_grid` (triggered by a row exceeding
    /// `GRID_COL_CAP`) must be folded into the caveat's elided total even
    /// when `elide_components` itself drops nothing. Before the fix,
    /// `elided` was computed before `place_grid` ran, so this fixture (well
    /// under `max_components` but over the per-row width cap) produced a
    /// caveat count of 0 despite silently dropping components, or no caveat
    /// line at all. This test would FAIL under that pre-fix ordering.
    #[test]
    fn grid_cap_drops_are_folded_into_elided_caveat_count() {
        let mut arena = NodeArena::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let edges = BidirectionalEdgeStore::new();
        let indices = AuxiliaryIndices::new();

        let f_web = files
            .register_with_language(Path::new("web/app.jsx"), Some(Language::JavaScript))
            .unwrap();
        let f_routes = files
            .register_with_language(Path::new("api/routes.rs"), Some(Language::Rust))
            .unwrap();
        let f_handlers = files
            .register_with_language(Path::new("api/handlers.rs"), Some(Language::Rust))
            .unwrap();

        let dashboard = node(
            &mut arena,
            &mut strings,
            NodeKind::Component,
            "Dashboard",
            "web::Dashboard",
            f_web,
            0,
        );
        let endpoint = node(
            &mut arena,
            &mut strings,
            NodeKind::Endpoint,
            "users_endpoint",
            "api::users_endpoint",
            f_routes,
            100,
        );
        let handler = node(
            &mut arena,
            &mut strings,
            NodeKind::Function,
            "handle_users",
            "api::handle_users",
            f_handlers,
            200,
        );

        let url = strings.intern("/users").unwrap();
        edges.add_edge(
            dashboard,
            endpoint,
            EdgeKind::HttpRequest {
                method: HttpMethod::Get,
                url: Some(url),
            },
            f_web,
        );
        edges.add_edge(
            endpoint,
            handler,
            EdgeKind::Calls {
                argument_count: 0,
                is_async: false,
                resolved_via: ResolvedVia::Direct,
            },
            f_routes,
        );

        // 15 distinct-file helper functions called directly by `handler`.
        // Each lands in its own file-scoped cluster with no non-default
        // type signal, so all classify as `Backend` (fallback) alongside
        // `endpoint` and `handler` -- 17 components sharing row 2, well
        // past `GRID_COL_CAP` (12), while the total component count (18)
        // stays far under `DEFAULT_MAX_COMPONENTS` (40) so
        // `elide_components` never fires.
        const HELPER_COUNT: usize = 15;
        for i in 0..HELPER_COUNT {
            let path = format!("svc/helper_{i}.rs");
            let f_helper = files
                .register_with_language(Path::new(&path), Some(Language::Rust))
                .unwrap();
            let name = format!("helper_{i}");
            let qname = format!("svc::helper_{i}");
            let helper = node(
                &mut arena,
                &mut strings,
                NodeKind::Function,
                &name,
                &qname,
                f_helper,
                300 + i as u32,
            );
            edges.add_edge(
                handler,
                helper,
                EdgeKind::Calls {
                    argument_count: 0,
                    is_async: false,
                    resolved_via: ResolvedVia::Direct,
                },
                f_handlers,
            );
        }

        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );

        let (sub, mut arch) = full_config();
        arch.seed_label = "web::Dashboard".to_string();
        let doc = build_archify_document(&graph.snapshot(), &[dashboard], &sub, &arch)
            .expect("archify doc");

        // Row-width invariant still holds: no row wider than the cap.
        let mut widths: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for c in &doc.components {
            *widths.entry(c.row).or_insert(0) += 1;
        }
        assert!(widths.values().all(|&w| w <= GRID_COL_CAP));

        // endpoint + handler + 15 helpers = 17 candidates for row 2; the cap
        // forces exactly 5 to be dropped at the grid stage, on top of the 0
        // that `elide_components` drops (18 total components <= 40).
        let backend_row_candidates = 2 + HELPER_COUNT;
        let expected_grid_drops = backend_row_candidates - GRID_COL_CAP;
        assert!(
            expected_grid_drops > 0,
            "fixture must actually exceed the grid cap"
        );

        let caveat_text = doc
            .cards
            .iter()
            .flat_map(|c| &c.items)
            .find(|i| i.starts_with("Elided "))
            .expect(
                "caveat card must report the grid-cap drops, not just elide_components' 0 count",
            );
        assert!(
            caveat_text.contains(&format!("Elided {expected_grid_drops} ")),
            "caveat must equal elide_components' drops (0 here) plus place_grid's grid-cap \
             drops (expected {expected_grid_drops}); got: {caveat_text}"
        );
    }

    // ---- fail-closed ----

    #[test]
    fn empty_subgraph_errors_closed() {
        let fx = build_fixture();
        let (sub, arch) = full_config();
        // No seeds -> empty subgraph -> NoComponents (never writes).
        let err = build_archify_document(&fx.graph.snapshot(), &[], &sub, &arch).unwrap_err();
        assert!(matches!(err, ArchifyError::NoComponents));
    }

    #[test]
    fn language_filter_to_nothing_errors_closed() {
        let fx = build_fixture();
        let arch = full_config().1;
        let sub = SeededSubgraphConfig {
            max_depth: 5,
            max_results: 1000,
            // Seed node is JavaScript; filter to a language not present.
            languages: vec![Language::Go],
        }
        .normalized();
        let err =
            build_archify_document(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch).unwrap_err();
        assert!(matches!(err, ArchifyError::NoComponents));
    }

    // ---- schema conformance (hermetic, vendored schema + jsonschema crate) ----

    struct CommonRetriever {
        common: serde_json::Value,
    }

    impl jsonschema::Retrieve for CommonRetriever {
        fn retrieve(
            &self,
            uri: &jsonschema::Uri<String>,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error + Send + Sync>> {
            if uri.as_str().ends_with("common.schema.json") {
                Ok(self.common.clone())
            } else {
                Err(format!("unexpected schema $ref: {uri}").into())
            }
        }
    }

    fn architecture_validator() -> jsonschema::Validator {
        let base = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../test-fixtures/archify-schema"
        );
        let arch: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{base}/architecture.schema.json")).unwrap(),
        )
        .unwrap();
        let common: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{base}/common.schema.json")).unwrap(),
        )
        .unwrap();
        jsonschema::options()
            .with_retriever(CommonRetriever { common })
            .build(&arch)
            .expect("build validator")
    }

    #[test]
    fn generated_samples_validate_against_vendored_schema() {
        let validator = architecture_validator();

        // Sample 1: full polyglot fixture.
        let fx = build_fixture();
        let (sub, arch) = full_config();
        let doc =
            build_archify_document(&fx.graph.snapshot(), &[fx.dashboard], &sub, &arch).unwrap();
        let value = serde_json::to_value(&doc).unwrap();
        assert!(
            validator.is_valid(&value),
            "polyglot sample failed schema: {:?}",
            validator
                .iter_errors(&value)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );

        // Sample 2: seed with no neighbours (single component).
        let mut arena = NodeArena::new();
        let mut strings = StringInterner::new();
        let mut files = FileRegistry::new();
        let edges = BidirectionalEdgeStore::new();
        let indices = AuxiliaryIndices::new();
        let f = files
            .register_with_language(Path::new("lib/solo.rs"), Some(Language::Rust))
            .unwrap();
        let solo = node(
            &mut arena,
            &mut strings,
            NodeKind::Function,
            "solo",
            "lib::solo",
            f,
            0,
        );
        let graph = CodeGraph::from_components(
            arena,
            edges,
            strings,
            files,
            indices,
            crate::graph::unified::NodeMetadataStore::new(),
        );
        let doc2 = build_archify_document(&graph.snapshot(), &[solo], &sub, &arch).unwrap();
        let value2 = serde_json::to_value(&doc2).unwrap();
        assert!(
            validator.is_valid(&value2),
            "solo sample failed schema: {:?}",
            validator
                .iter_errors(&value2)
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
        );

        // Sample 3: limit-forced elision.
        let (sub3, mut arch3) = full_config();
        arch3.max_components = 2;
        let doc3 =
            build_archify_document(&fx.graph.snapshot(), &[fx.dashboard], &sub3, &arch3).unwrap();
        let value3 = serde_json::to_value(&doc3).unwrap();
        assert!(validator.is_valid(&value3), "elided sample failed schema");
    }
}
