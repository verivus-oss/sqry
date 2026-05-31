//! Pass 5: Cross-Language Linking — Connect placeholder nodes across files and languages.
//!
//! This module implements a post-build pass that links placeholder nodes (created by
//! per-file graph builders) to their real implementations across files and languages.
//!
//! # Overview
//!
//! During per-file graph building (Passes 1-4), language plugins create placeholder
//! nodes for cross-language references:
//! - **FFI**: Rust `extern "C" { fn puts(); }` creates a stub `extern::C::puts` node
//! - **HTTP**: JS `fetch("/api/users")` creates an `HttpRequest` edge to a stub module
//!
//! Pass 5 runs after all files are processed and connects these placeholders to the
//! actual target implementations:
//! - **FFI**: `extern::C::puts` → C function `puts` in another file
//! - **HTTP**: `fetch("/api/users")` source → `route::GET::/api/users` Endpoint node
//!
//! # Algorithm
//!
//! The pass operates in two phases:
//!
//! ## FFI Linking
//! 1. Scan all Function nodes for FFI declaration patterns (`extern::*`, `C.*`, etc.)
//! 2. Build a lookup map of C/C++ function names
//! 3. Match FFI declarations to C/C++ implementations
//! 4. Create `FfiCall` edges from declarations to implementations
//!
//! ## HTTP Linking
//! 1. Collect all `Endpoint` nodes (created by plugin route detection)
//! 2. Scan all `HttpRequest` edges
//! 3. Match requests to endpoints by HTTP method and normalized URL path
//! 4. Create new `HttpRequest` edges from request sources to matched endpoints

use std::collections::HashMap;

use crate::graph::node::Language;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::edge::EdgeKind;
use crate::graph::unified::edge::kind::{FfiConvention, HttpMethod};
use crate::graph::unified::file::FileId;
use crate::graph::unified::mutation_target::GraphMutationTarget;
use crate::graph::unified::node::{NodeId, NodeKind};
use crate::graph::unified::storage::NodeEntry;

/// Statistics from Pass 5 execution.
#[derive(Debug, Clone, Default)]
pub struct Pass5Stats {
    /// Number of FFI declaration nodes scanned.
    pub ffi_declarations_scanned: usize,
    /// Number of FFI edges created (linking declarations to implementations).
    pub ffi_edges_created: usize,
    /// Number of HTTP request edges scanned.
    pub http_requests_scanned: usize,
    /// Number of HTTP request-to-endpoint matches found.
    pub http_endpoints_matched: usize,
    /// Total cross-language edges created (FFI + HTTP).
    pub total_edges_created: usize,
}

/// A pending cross-language edge to be added to the graph.
#[derive(Debug, Clone)]
struct PendingCrossLanguageEdge {
    /// Source node of the edge.
    source: NodeId,
    /// Target node of the edge.
    target: NodeId,
    /// Edge kind (`FfiCall` or `HttpRequest`).
    kind: EdgeKind,
    /// File containing the source node.
    file: FileId,
}

/// Run Pass 5: link cross-language edges in the completed graph.
///
/// This function operates on the fully-built `CodeGraph` after all files have been
/// processed through Passes 1-4. It scans for placeholder nodes and edges, then
/// creates new edges linking them to real implementations across files and languages.
///
/// # Arguments
///
/// * `graph` - Mutable reference to the completed code graph
///
/// # Returns
///
/// Statistics about the linking operation.
///
/// # Public shim
///
/// This is the `&mut CodeGraph` entry point external callers (tests,
/// snapshot loaders) rely on. Delegates to [`link_cross_language_edges_generic`]
/// which carries the `G: GraphMutationTarget` bound. The shim keeps the
/// trait `pub(crate)` so the incremental rebuild plane remains
/// daemon-internal.
pub fn link_cross_language_edges(graph: &mut CodeGraph) -> Pass5Stats {
    link_cross_language_edges_generic(graph)
}

/// Generic implementation used by both the public [`link_cross_language_edges`]
/// shim (full build path) and the intra-crate incremental rebuild
/// dispatcher (Task 4 Step 4 Phase 3, operating on a [`RebuildGraph`]).
///
/// [`RebuildGraph`]: crate::graph::unified::rebuild::rebuild_graph::RebuildGraph
pub(crate) fn link_cross_language_edges_generic<G: GraphMutationTarget>(
    graph: &mut G,
) -> Pass5Stats {
    let mut stats = Pass5Stats::default();
    let mut pending_edges: Vec<PendingCrossLanguageEdge> = Vec::new();

    // Phase 1: FFI linking
    link_ffi_edges(graph, &mut stats, &mut pending_edges);

    // Phase 2: HTTP linking
    link_http_edges(graph, &mut stats, &mut pending_edges);

    // Apply all pending edges to the graph
    for edge in &pending_edges {
        graph.edges_mut().add_edge_with_spans(
            edge.source,
            edge.target,
            edge.kind.clone(),
            edge.file,
            vec![],
        );
    }

    stats.total_edges_created = stats.ffi_edges_created + stats.http_endpoints_matched;

    if stats.total_edges_created > 0 {
        log::info!(
            "Pass 5: created {} cross-language edges ({} FFI, {} HTTP)",
            stats.total_edges_created,
            stats.ffi_edges_created,
            stats.http_endpoints_matched,
        );
    }

    stats
}

// ============================================================================
// FFI Linking
// ============================================================================

/// Collected FFI declaration from the graph.
#[derive(Debug)]
struct FfiDeclaration {
    /// The node ID of the FFI declaration.
    node_id: NodeId,
    /// The bare function name to match against (for example, `puts` from `extern::C::puts`).
    bare_name: String,
    /// The FFI calling convention.
    convention: FfiConvention,
    /// The file containing this declaration.
    file_id: FileId,
}

/// Extract the bare function name and convention from an FFI qualified name.
///
/// Supported patterns:
/// - `extern::C::puts` → ("puts", C)
/// - `extern::stdcall::func` → ("func", Stdcall)
/// - `extern::system::func` → ("func", System)
/// - `C.puts` → ("puts", C)
/// - `native::jni::func` → ("func", C)
/// - `native::ctypes::func` → ("func", C)
/// - `native::cffi::func` → ("func", C)
/// - `native::ffi::func` → ("func", C)
/// - `native::panama::func` → ("func", C)
fn parse_ffi_qualified_name(qualified_name: &str) -> Option<(String, FfiConvention)> {
    if let Some(rest) = qualified_name.strip_prefix("extern::") {
        // Rust-style: extern::C::puts, extern::stdcall::func
        if let Some(pos) = rest.find("::") {
            let convention_str = &rest[..pos];
            let bare_name = &rest[pos + 2..];
            if bare_name.is_empty() {
                return None;
            }
            let convention = match convention_str {
                "cdecl" => FfiConvention::Cdecl,
                "stdcall" => FfiConvention::Stdcall,
                "fastcall" => FfiConvention::Fastcall,
                "system" => FfiConvention::System,
                _ => FfiConvention::C,
            };
            return Some((bare_name.to_string(), convention));
        }
    } else if let Some(rest) = qualified_name
        .strip_prefix("C.")
        .or_else(|| qualified_name.strip_prefix("C::"))
    {
        // Go CGo style: C.puts or C::puts
        if !rest.is_empty() {
            return Some((rest.to_string(), FfiConvention::C));
        }
    } else if let Some(rest) = qualified_name.strip_prefix("native::jni::") {
        // Java JNI style
        if !rest.is_empty() {
            return Some((rest.to_string(), FfiConvention::C));
        }
    } else if let Some(rest) = qualified_name
        .strip_prefix("native::ctypes::")
        .or_else(|| qualified_name.strip_prefix("native::cffi::"))
        .or_else(|| qualified_name.strip_prefix("native::ffi::"))
        .or_else(|| qualified_name.strip_prefix("native::panama::"))
    {
        // Python ctypes/cffi, PHP FFI, Java Panama style
        if !rest.is_empty() {
            return Some((rest.to_string(), FfiConvention::C));
        }
    }

    None
}

/// Convert a Java fully-qualified method name to JNI C function name.
///
/// Example: `com.example.Class.method` → `Java_com_example_Class_method`
///
/// JNI convention: replace dots with underscores and prepend `Java_`.
fn java_to_jni_c_name(java_name: &str) -> String {
    let joined_segments = java_name
        .split("::")
        .flat_map(|segment| segment.split('.'))
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    format!("Java_{joined_segments}")
}

/// Return whether a Java name has package/class qualification segments.
fn is_java_qualified_name(java_name: &str) -> bool {
    java_name.contains('.') || java_name.contains("::")
}

/// Return the last segment from a Java qualified method name.
fn java_method_name(java_name: &str) -> Option<&str> {
    java_name
        .rsplit("::")
        .next()
        .and_then(|segment| segment.rsplit('.').next())
        .filter(|segment| !segment.is_empty())
}

/// Collect FFI declarations from the graph and match them to C/C++ implementations.
fn link_ffi_edges<G: GraphMutationTarget>(
    graph: &G,
    stats: &mut Pass5Stats,
    pending: &mut Vec<PendingCrossLanguageEdge>,
) {
    // Step 1: Collect all FFI declarations
    let ffi_declarations = collect_ffi_declarations(graph, stats);
    if ffi_declarations.is_empty() {
        return;
    }

    // Step 2: Build C/C++ function lookup map
    let c_functions = build_c_function_map(graph);
    if c_functions.is_empty() {
        return;
    }

    // Step 3: Match FFI declarations to C/C++ implementations
    for decl in &ffi_declarations {
        // Try direct name match first
        let mut matched = try_ffi_match(decl, &c_functions, stats, pending);

        // For JNI declarations, try JNI-mangled name as fallback
        if !matched && is_java_qualified_name(&decl.bare_name) {
            let jni_name = java_to_jni_c_name(&decl.bare_name);
            let jni_decl = FfiDeclaration {
                node_id: decl.node_id,
                bare_name: jni_name,
                convention: decl.convention,
                file_id: decl.file_id,
            };
            matched = try_ffi_match(&jni_decl, &c_functions, stats, pending);
        }

        // For JNI declarations, also try bare method name (last segment after final dot)
        if !matched && let Some(method_name) = java_method_name(&decl.bare_name) {
            let bare_decl = FfiDeclaration {
                node_id: decl.node_id,
                bare_name: method_name.to_string(),
                convention: decl.convention,
                file_id: decl.file_id,
            };
            try_ffi_match(&bare_decl, &c_functions, stats, pending);
        }
    }
}

/// Try to match an FFI declaration against the C function map.
///
/// Returns `true` if at least one match was found.
fn try_ffi_match(
    decl: &FfiDeclaration,
    c_functions: &HashMap<String, Vec<(NodeId, FileId)>>,
    stats: &mut Pass5Stats,
    pending: &mut Vec<PendingCrossLanguageEdge>,
) -> bool {
    let Some(targets) = c_functions.get(&decl.bare_name) else {
        return false;
    };
    let mut found = false;
    for &(target_node, target_file) in targets {
        // Skip same-file matches (these are already handled by per-file passes)
        if target_file == decl.file_id {
            continue;
        }
        found = true;
        stats.ffi_edges_created += 1;
        pending.push(PendingCrossLanguageEdge {
            source: decl.node_id,
            target: target_node,
            kind: EdgeKind::FfiCall {
                convention: decl.convention,
            },
            file: decl.file_id,
        });
    }
    found
}

/// Resolve the graph-structural identity for a node.
///
/// Cross-language linking operates on canonical graph names when available,
/// because semantic display names may intentionally collapse qualified markers
/// such as `extern::C::` or `native::jni::`.
fn entry_structural_name<G: GraphMutationTarget>(
    graph: &G,
    entry: &NodeEntry,
) -> Option<std::sync::Arc<str>> {
    entry
        .qualified_name
        .and_then(|qualified_name_id| graph.strings().resolve(qualified_name_id))
        .or_else(|| graph.strings().resolve(entry.name))
}

/// Collect FFI declaration nodes from the graph.
fn collect_ffi_declarations<G: GraphMutationTarget>(
    graph: &G,
    stats: &mut Pass5Stats,
) -> Vec<FfiDeclaration> {
    let mut declarations = Vec::new();
    let function_nodes = graph.indices().by_kind(NodeKind::Function);

    for &node_id in function_nodes {
        let Some(entry) = graph.nodes().get(node_id) else {
            continue;
        };

        let Some(name_str) = entry_structural_name(graph, entry) else {
            continue;
        };

        if let Some((bare_name, convention)) = parse_ffi_qualified_name(&name_str) {
            stats.ffi_declarations_scanned += 1;
            declarations.push(FfiDeclaration {
                node_id,
                bare_name,
                convention,
                file_id: entry.file,
            });
        }
    }

    declarations
}

/// Build a lookup map of C/C++ function names to their node IDs and file IDs.
fn build_c_function_map<G: GraphMutationTarget>(
    graph: &G,
) -> HashMap<String, Vec<(NodeId, FileId)>> {
    let mut map: HashMap<String, Vec<(NodeId, FileId)>> = HashMap::new();

    for lang in &[Language::C, Language::Cpp] {
        let files = graph.files().files_by_language(*lang);
        for (file_id, _path) in files {
            let file_nodes = graph.indices().by_file(file_id);
            for &node_id in file_nodes {
                let Some(entry) = graph.nodes().get(node_id) else {
                    continue;
                };
                if entry.kind != NodeKind::Function {
                    continue;
                }
                let Some(name_str) = entry_structural_name(graph, entry) else {
                    continue;
                };
                // Use the bare name (last segment after :: or the whole name)
                let bare_name = name_str
                    .rsplit("::")
                    .next()
                    .unwrap_or(&name_str)
                    .to_string();
                map.entry(bare_name).or_default().push((node_id, file_id));
            }
        }
    }

    map
}

// ============================================================================
// HTTP Linking
// ============================================================================

/// Parsed endpoint information from a route node.
#[derive(Debug)]
struct EndpointInfo {
    /// The node ID of the endpoint.
    node_id: NodeId,
    /// The HTTP method (GET, POST, etc.).
    method: HttpMethod,
    /// The normalized URL path.
    normalized_path: String,
    /// The file containing this endpoint.
    file_id: FileId,
}

/// Collected HTTP request edge information.
#[derive(Debug)]
struct HttpRequestInfo {
    /// The source node making the request.
    source_node: NodeId,
    /// The HTTP method.
    method: HttpMethod,
    /// The URL path (raw, before normalization).
    url_path: String,
    /// The file containing the request.
    file_id: FileId,
}

/// Parse a route endpoint qualified name into method and path.
///
/// Expected format: `route::GET::/api/users` or `route::POST::/api/items`
fn parse_endpoint_qualified_name(qualified_name: &str) -> Option<(HttpMethod, String)> {
    let rest = qualified_name.strip_prefix("route::")?;
    let sep = rest.find("::")?;
    let method_str = &rest[..sep];
    let path = &rest[sep + 2..];

    let method = match method_str {
        "GET" => HttpMethod::Get,
        "POST" => HttpMethod::Post,
        "PUT" => HttpMethod::Put,
        "DELETE" => HttpMethod::Delete,
        "PATCH" => HttpMethod::Patch,
        "HEAD" => HttpMethod::Head,
        "OPTIONS" => HttpMethod::Options,
        "ALL" => HttpMethod::All,
        _ => return None,
    };

    Some((method, path.to_string()))
}

/// Normalize a URL path for matching.
///
/// - Strips leading and trailing slashes
/// - Collapses double slashes
/// - Normalizes parameter syntax: `{id}`, `<id>`, `[id]` → `:id`
/// - Strips query strings
#[must_use]
pub fn normalize_url_path(path: &str) -> String {
    // Strip query string
    let path = path.split('?').next().unwrap_or(path);

    // Strip leading/trailing slashes
    let path = path.trim_matches('/');

    // Split into segments, filter empty (collapses double slashes)
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // Normalize parameter syntax in each segment
    let normalized: Vec<String> = segments
        .into_iter()
        .map(|seg| {
            // {id} → :id
            if seg.starts_with('{') && seg.ends_with('}') {
                return format!(":{}", &seg[1..seg.len() - 1]);
            }
            // <id> or <int:id> → :id
            if seg.starts_with('<') && seg.ends_with('>') {
                let inner = &seg[1..seg.len() - 1];
                // Handle typed params like <int:id>, <path:subpath>
                let param_name = if let Some(pos) = inner.find(':') {
                    &inner[pos + 1..]
                } else {
                    inner
                };
                return format!(":{param_name}");
            }
            // [id] → :id
            if seg.starts_with('[') && seg.ends_with(']') {
                return format!(":{}", &seg[1..seg.len() - 1]);
            }
            // Already :id format — keep as-is
            seg.to_string()
        })
        .collect();

    normalized.join("/")
}

/// Collect endpoints and HTTP requests, then match them.
fn link_http_edges<G: GraphMutationTarget>(
    graph: &G,
    stats: &mut Pass5Stats,
    pending: &mut Vec<PendingCrossLanguageEdge>,
) {
    // Step 1: Collect all endpoints
    let endpoints = collect_endpoints(graph);
    if endpoints.is_empty() {
        return;
    }

    // Build endpoint lookup: (method, normalized_path) → Vec<(NodeId, FileId)>
    let mut endpoint_map: HashMap<(HttpMethod, String), Vec<(NodeId, FileId)>> = HashMap::new();
    for ep in &endpoints {
        endpoint_map
            .entry((ep.method, ep.normalized_path.clone()))
            .or_default()
            .push((ep.node_id, ep.file_id));
    }

    // Step 2: Collect all HTTP request edges
    let requests = collect_http_requests(graph, stats);
    if requests.is_empty() {
        return;
    }

    // Step 3: Match requests to endpoints
    for req in &requests {
        let normalized = normalize_url_path(&req.url_path);

        // Exact match: same method and path
        try_http_match(req, &normalized, req.method, &endpoint_map, stats, pending);

        // Wildcard matching: if the request uses All, also check specific-method endpoints
        if req.method == HttpMethod::All {
            for specific_method in &[
                HttpMethod::Get,
                HttpMethod::Post,
                HttpMethod::Put,
                HttpMethod::Delete,
                HttpMethod::Patch,
                HttpMethod::Head,
                HttpMethod::Options,
            ] {
                try_http_match(
                    req,
                    &normalized,
                    *specific_method,
                    &endpoint_map,
                    stats,
                    pending,
                );
            }
        } else {
            // If request is a specific method, also check for All endpoints
            try_http_match(
                req,
                &normalized,
                HttpMethod::All,
                &endpoint_map,
                stats,
                pending,
            );
        }
    }
}

/// Try to match an HTTP request against endpoints with a given method and path.
fn try_http_match(
    req: &HttpRequestInfo,
    normalized_path: &str,
    lookup_method: HttpMethod,
    endpoint_map: &HashMap<(HttpMethod, String), Vec<(NodeId, FileId)>>,
    stats: &mut Pass5Stats,
    pending: &mut Vec<PendingCrossLanguageEdge>,
) {
    let key = (lookup_method, normalized_path.to_string());
    if let Some(targets) = endpoint_map.get(&key) {
        for &(target_node, target_file) in targets {
            // Skip same-file matches
            if target_file == req.file_id {
                continue;
            }
            stats.http_endpoints_matched += 1;
            pending.push(PendingCrossLanguageEdge {
                source: req.source_node,
                target: target_node,
                kind: EdgeKind::HttpRequest {
                    method: req.method,
                    url: None, // Cross-file edges don't carry the URL StringId
                },
                file: req.file_id,
            });
        }
    }
}

/// Collect all Endpoint nodes from the graph.
fn collect_endpoints<G: GraphMutationTarget>(graph: &G) -> Vec<EndpointInfo> {
    let mut endpoints = Vec::new();
    let endpoint_nodes = graph.indices().by_kind(NodeKind::Endpoint);

    for &node_id in endpoint_nodes {
        let Some(entry) = graph.nodes().get(node_id) else {
            continue;
        };
        let Some(name_str) = entry_structural_name(graph, entry) else {
            continue;
        };

        if let Some((method, path)) = parse_endpoint_qualified_name(&name_str) {
            let normalized = normalize_url_path(&path);
            endpoints.push(EndpointInfo {
                node_id,
                method,
                normalized_path: normalized,
                file_id: entry.file,
            });
        }
    }

    endpoints
}

/// Collect all HTTP request edges from the graph — across CSR **and** delta.
///
/// Before Phase 3e's §E-harness CSR normalisation, incremental rebuilds were
/// compared to uncompacted delta-only full rebuilds, so scanning the delta
/// buffer alone happened to cover every `HttpRequest` edge in both sides of
/// the comparison. That invariant broke the moment Phase 3e + the harness
/// changes began comparing CSR-compacted graphs — unchanged `HttpRequest`
/// edges live in the CSR tier after `RebuildGraph::finalize()` (or after
/// `persist_and_analyze_graph`'s compaction for the daemon's load path), so
/// a delta-only scan silently dropped them and Pass 5 stopped relinking
/// valid cross-file HTTP requests. The §E harness surfaces this as e.g.
/// `client.ts -> route::POST::/api/users` missing in the candidate on the
/// `ts_http_routes / AddHttpRoute{server.ts}` shrink.
///
/// # Why a single-pass edge-store iterator
///
/// An earlier iteration of this function walked every live node and called
/// `graph.edges().edges_from(source_node)` per node. That is O(N + E) on
/// a CSR-backed graph but `O(N * |delta|)` on a delta-only graph because
/// `edges_from` rebuilds its per-source LWW map from the entire delta
/// buffer on every invocation. Pass 5 runs in both the full-build pipeline
/// (delta-only — CSR compaction happens later in `persist_and_analyze_graph`
/// or `RebuildGraph::finalize` step 9) and the incremental pipeline
/// (mixed CSR + delta), so the per-node loop is a full-build quadratic
/// regression.
///
/// The fix: drive this scan through
/// [`BidirectionalEdgeStore::all_live_forward_edges`], which builds the
/// delta LWW map **once** and walks CSR once, giving `O(|csr| + |delta|)`
/// total work on every graph shape. Asymptotic parity with the old
/// delta-only `forward.delta().iter()` scan is restored for full builds
/// while CSR-backed edges remain visible for incremental rebuilds.
///
/// # Filter + payload resolution semantics
///
/// For every live forward edge we check the `EdgeKind::HttpRequest`
/// discriminant, resolve the URL payload through the string interner,
/// and resolve the edge source's owning file via the node arena. Edges
/// whose source slot has been tombstoned (e.g. `remove_file` during the
/// rebuild plane) are filtered out of the arena by `NodeArena::get`.
///
/// Determinism: CSR edges are yielded in `(source_index, row_ptr)`
/// order (dense, stable); delta Adds in `HashMap` iteration order
/// (unordered). Pass 5's downstream linker builds a lookup table keyed
/// by `(method, normalized_path)` so the intra-tier iteration order is
/// immaterial to the linker's output.
fn collect_http_requests<G: GraphMutationTarget>(
    graph: &G,
    stats: &mut Pass5Stats,
) -> Vec<HttpRequestInfo> {
    let mut requests = Vec::new();

    for edge_ref in graph.edges().all_live_forward_edges() {
        let EdgeKind::HttpRequest { method, url } = &edge_ref.kind else {
            continue;
        };
        stats.http_requests_scanned += 1;
        let Some(url_id) = url else {
            continue;
        };
        let Some(url_str) = graph.strings().resolve(*url_id) else {
            continue;
        };
        // Resolve the source's owning file via the arena. CSR-backed edges
        // are emitted by `all_live_forward_edges` with the source's
        // generation hard-coded to 0 (CSR is keyed by slot index, not by
        // generation), so a strict `nodes().get(edge_ref.source)` lookup
        // misses any source whose arena slot has been re-allocated to a
        // higher generation by an earlier incremental rebuild — even
        // though the underlying slot still holds the same live `NodeEntry`
        // that produced the edge. Fall back to a slot-index lookup that
        // ignores the generation so cross-language HTTP linking survives
        // a tombstone-and-re-allocate sweep. This matches the semantic
        // contract documented at `EdgeStore::all_live_forward_edges`: the
        // edges it returns are live by construction (CSR tombstones +
        // delta shadows filtered), so the slot is guaranteed to be
        // occupied — only the generation is ambiguous.
        let (source_node, source_file) = match graph.nodes().get(edge_ref.source) {
            Some(entry) => (edge_ref.source, entry.file),
            None => {
                let slot = graph.nodes().slot(edge_ref.source.index());
                let Some(slot) = slot else { continue };
                let Some(entry) = slot.get() else { continue };
                // Rewire the source NodeId to carry the slot's live
                // generation so downstream consumers (e.g. the
                // cross-file edge Pass 5 emits) reference a NodeId that
                // will still be valid after `RebuildGraph::finalize` step
                // 9 rebuilds the CSR from compacted deltas.
                let live_source = NodeId::new(edge_ref.source.index(), slot.generation());
                (live_source, entry.file)
            }
        };
        requests.push(HttpRequestInfo {
            source_node,
            method: *method,
            url_path: url_str.to_string(),
            file_id: source_file,
        });
    }

    requests
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- URL normalization tests ----

    #[test]
    fn test_normalize_url_path_basic() {
        assert_eq!(normalize_url_path("/api/users"), "api/users");
        assert_eq!(normalize_url_path("/api/users/"), "api/users");
        assert_eq!(normalize_url_path("api/users"), "api/users");
    }

    #[test]
    fn test_normalize_url_path_params() {
        assert_eq!(normalize_url_path("/api/users/{id}"), "api/users/:id");
        assert_eq!(normalize_url_path("/api/users/<id>"), "api/users/:id");
        assert_eq!(normalize_url_path("/api/users/[id]"), "api/users/:id");
        assert_eq!(normalize_url_path("/api/users/:id"), "api/users/:id");
    }

    #[test]
    fn test_normalize_url_path_double_slashes() {
        assert_eq!(normalize_url_path("/api//users"), "api/users");
        assert_eq!(normalize_url_path("///api///users///"), "api/users");
    }

    #[test]
    fn test_normalize_url_path_query_string() {
        assert_eq!(
            normalize_url_path("/api/users?page=1&limit=10"),
            "api/users"
        );
    }

    #[test]
    fn test_normalize_url_path_empty() {
        assert_eq!(normalize_url_path("/"), "");
        assert_eq!(normalize_url_path(""), "");
    }

    // ---- FFI qualified name parsing tests ----

    #[test]
    fn test_parse_ffi_rust_extern_c() {
        let result = parse_ffi_qualified_name("extern::C::puts");
        assert_eq!(result, Some(("puts".to_string(), FfiConvention::C)));
    }

    #[test]
    fn test_parse_ffi_rust_extern_stdcall() {
        let result = parse_ffi_qualified_name("extern::stdcall::MessageBoxA");
        assert_eq!(
            result,
            Some(("MessageBoxA".to_string(), FfiConvention::Stdcall))
        );
    }

    #[test]
    fn test_parse_ffi_go_cgo() {
        let result = parse_ffi_qualified_name("C.puts");
        assert_eq!(result, Some(("puts".to_string(), FfiConvention::C)));
    }

    #[test]
    fn test_parse_ffi_java_jni() {
        let result = parse_ffi_qualified_name("native::jni::Java_MyClass_doStuff");
        assert_eq!(
            result,
            Some(("Java_MyClass_doStuff".to_string(), FfiConvention::C))
        );
    }

    #[test]
    fn test_parse_ffi_python_ctypes() {
        let result = parse_ffi_qualified_name("native::ctypes::calculate");
        assert_eq!(result, Some(("calculate".to_string(), FfiConvention::C)));
    }

    #[test]
    fn test_parse_ffi_python_cffi() {
        let result = parse_ffi_qualified_name("native::cffi::calculate");
        assert_eq!(result, Some(("calculate".to_string(), FfiConvention::C)));
    }

    #[test]
    fn test_parse_ffi_php_ffi() {
        let result = parse_ffi_qualified_name("native::ffi::crypto_encrypt");
        assert_eq!(
            result,
            Some(("crypto_encrypt".to_string(), FfiConvention::C))
        );
    }

    #[test]
    fn test_parse_ffi_java_panama() {
        let result = parse_ffi_qualified_name("native::panama::nativeLinker");
        assert_eq!(result, Some(("nativeLinker".to_string(), FfiConvention::C)));
    }

    #[test]
    fn test_parse_ffi_not_ffi() {
        assert!(parse_ffi_qualified_name("main").is_none());
        assert!(parse_ffi_qualified_name("module::func").is_none());
        assert!(parse_ffi_qualified_name("").is_none());
    }

    #[test]
    fn test_parse_ffi_edge_cases() {
        // Empty bare name
        assert!(parse_ffi_qualified_name("extern::C::").is_none());
        assert!(parse_ffi_qualified_name("C.").is_none());
    }

    // ---- Endpoint qualified name parsing tests ----

    #[test]
    fn test_parse_endpoint_get() {
        let result = parse_endpoint_qualified_name("route::GET::/api/users");
        assert_eq!(result, Some((HttpMethod::Get, "/api/users".to_string())));
    }

    #[test]
    fn test_parse_endpoint_post() {
        let result = parse_endpoint_qualified_name("route::POST::/api/items");
        assert_eq!(result, Some((HttpMethod::Post, "/api/items".to_string())));
    }

    #[test]
    fn test_parse_endpoint_all_methods() {
        assert!(matches!(
            parse_endpoint_qualified_name("route::PUT::/x"),
            Some((HttpMethod::Put, _))
        ));
        assert!(matches!(
            parse_endpoint_qualified_name("route::DELETE::/x"),
            Some((HttpMethod::Delete, _))
        ));
        assert!(matches!(
            parse_endpoint_qualified_name("route::PATCH::/x"),
            Some((HttpMethod::Patch, _))
        ));
        assert!(matches!(
            parse_endpoint_qualified_name("route::HEAD::/x"),
            Some((HttpMethod::Head, _))
        ));
        assert!(matches!(
            parse_endpoint_qualified_name("route::OPTIONS::/x"),
            Some((HttpMethod::Options, _))
        ));
    }

    #[test]
    fn test_parse_endpoint_invalid() {
        assert!(parse_endpoint_qualified_name("not_a_route").is_none());
        assert!(parse_endpoint_qualified_name("route::INVALID::/x").is_none());
    }

    // ---- Graph-level unit tests ----

    #[test]
    fn test_link_cross_language_edges_empty_graph() {
        let mut graph = CodeGraph::new();
        let stats = link_cross_language_edges(&mut graph);
        assert_eq!(stats.total_edges_created, 0);
        assert_eq!(stats.ffi_declarations_scanned, 0);
        assert_eq!(stats.http_requests_scanned, 0);
    }

    #[test]
    fn test_ffi_matching_with_real_graph() {
        use crate::graph::unified::build::helper::GraphBuildHelper;
        use crate::graph::unified::build::staging::StagingGraph;
        use std::path::PathBuf;

        let mut graph = CodeGraph::new();

        // File 1: Rust file with extern declaration
        let rust_path = PathBuf::from("/test/bindings.rs");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &rust_path, Language::Rust);
            let _extern_fn = helper.add_function("extern::C::calculate_sum", None, false, false);
            commit_staging_to_graph(&mut graph, &rust_path, Language::Rust, staging);
        }

        // File 2: C file with implementation
        let c_path = PathBuf::from("/test/math.c");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &c_path, Language::C);
            let _c_fn = helper.add_function("calculate_sum", None, false, false);
            commit_staging_to_graph(&mut graph, &c_path, Language::C, staging);
        }

        // Run Pass 5
        let stats = link_cross_language_edges(&mut graph);

        assert_eq!(stats.ffi_declarations_scanned, 1);
        assert_eq!(stats.ffi_edges_created, 1);
        assert_eq!(stats.total_edges_created, 1);
    }

    #[test]
    fn test_http_matching_with_real_graph() {
        use crate::graph::unified::build::helper::GraphBuildHelper;
        use crate::graph::unified::build::staging::StagingGraph;
        use crate::graph::unified::edge::kind::HttpMethod;
        use std::path::PathBuf;

        let mut graph = CodeGraph::new();

        // File 1: Python server with endpoint
        let server_path = PathBuf::from("/test/server.py");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &server_path, Language::Python);
            let _endpoint = helper.add_endpoint("route::GET::/api/users", None);
            commit_staging_to_graph(&mut graph, &server_path, Language::Python, staging);
        }

        // File 2: JavaScript client with HTTP request
        let client_path = PathBuf::from("/test/client.js");
        {
            let mut staging = StagingGraph::new();
            let mut helper =
                GraphBuildHelper::new(&mut staging, &client_path, Language::JavaScript);
            let fetch_fn = helper.add_function("fetchUsers", None, true, false);
            let target = helper.add_module("http::/api/users", None);
            helper.add_http_request_edge(fetch_fn, target, HttpMethod::Get, Some("/api/users"));
            commit_staging_to_graph(&mut graph, &client_path, Language::JavaScript, staging);
        }

        // Run Pass 5
        let stats = link_cross_language_edges(&mut graph);

        assert_eq!(stats.http_requests_scanned, 1);
        assert_eq!(stats.http_endpoints_matched, 1);
        assert_eq!(stats.total_edges_created, 1);
    }

    #[test]
    fn test_ffi_no_match_same_file() {
        use crate::graph::unified::build::helper::GraphBuildHelper;
        use crate::graph::unified::build::staging::StagingGraph;
        use std::path::PathBuf;

        let mut graph = CodeGraph::new();

        // Same file has both extern and implementation — should NOT create cross-file edge
        let path = PathBuf::from("/test/combined.c");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &path, Language::C);
            let _extern_fn = helper.add_function("extern::C::my_func", None, false, false);
            let _impl_fn = helper.add_function("my_func", None, false, false);
            commit_staging_to_graph(&mut graph, &path, Language::C, staging);
        }

        let stats = link_cross_language_edges(&mut graph);
        assert_eq!(stats.ffi_edges_created, 0);
    }

    /// Helper to commit a staging graph to the main `CodeGraph`.
    ///
    /// This mirrors the logic in `entrypoint.rs::process_file` but simplified for tests.
    fn commit_staging_to_graph(
        graph: &mut CodeGraph,
        path: &std::path::Path,
        language: Language,
        mut staging: crate::graph::unified::build::staging::StagingGraph,
    ) {
        let file_id = graph
            .files_mut()
            .register_with_language(path, Some(language))
            .expect("register file");

        staging.apply_file_id(file_id);

        let string_remap = staging
            .commit_strings(graph.strings_mut())
            .expect("commit strings");
        staging
            .apply_string_remap(&string_remap)
            .expect("apply string remap");

        let node_id_mapping = staging
            .commit_nodes(graph.nodes_mut())
            .expect("commit nodes");

        // Update indices
        let index_entries: Vec<_> = node_id_mapping
            .values()
            .filter_map(|&actual_id| {
                graph.nodes().get(actual_id).map(|entry| {
                    (
                        actual_id,
                        entry.kind,
                        entry.name,
                        entry.qualified_name,
                        entry.file,
                    )
                })
            })
            .collect();
        for (node_id, kind, name, qualified_name, file) in index_entries {
            graph
                .indices_mut()
                .add(node_id, kind, name, qualified_name, file);
        }

        // Commit edges
        let edges = staging.get_remapped_edges(&node_id_mapping);
        for edge in edges {
            graph.edges_mut().add_edge_with_spans(
                edge.source,
                edge.target,
                edge.kind.clone(),
                file_id,
                edge.spans.clone(),
            );
        }
    }

    // ---- F1: Go CGo C:: prefix tests ----

    #[test]
    fn test_parse_ffi_go_cgo_double_colon() {
        let result = parse_ffi_qualified_name("C::puts");
        assert_eq!(result, Some(("puts".to_string(), FfiConvention::C)));
    }

    #[test]
    fn test_parse_ffi_go_cgo_double_colon_empty() {
        assert!(parse_ffi_qualified_name("C::").is_none());
    }

    // ---- F2: JNI name demangling tests ----

    #[test]
    fn test_java_to_jni_c_name() {
        assert_eq!(
            java_to_jni_c_name("com.example.Class.method"),
            "Java_com_example_Class_method"
        );
        assert_eq!(
            java_to_jni_c_name("MyClass.doStuff"),
            "Java_MyClass_doStuff"
        );
        assert_eq!(
            java_to_jni_c_name("com::example::Class::method"),
            "Java_com_example_Class_method"
        );
    }

    #[test]
    fn test_ffi_jni_demangling_with_real_graph() {
        use crate::graph::unified::build::helper::GraphBuildHelper;
        use crate::graph::unified::build::staging::StagingGraph;
        use std::path::PathBuf;

        let mut graph = CodeGraph::new();

        // File 1: Java JNI declaration with fully-qualified name
        let java_path = PathBuf::from("/test/MyClass.java");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &java_path, Language::Java);
            let _jni_fn = helper.add_function(
                "native::jni::com.example.MyClass.doStuff",
                None,
                false,
                false,
            );
            commit_staging_to_graph(&mut graph, &java_path, Language::Java, staging);
        }

        // File 2: C file with JNI-mangled implementation
        let c_path = PathBuf::from("/test/jni_impl.c");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &c_path, Language::C);
            let _c_fn = helper.add_function("Java_com_example_MyClass_doStuff", None, false, false);
            commit_staging_to_graph(&mut graph, &c_path, Language::C, staging);
        }

        let stats = link_cross_language_edges(&mut graph);
        assert_eq!(stats.ffi_declarations_scanned, 1);
        assert_eq!(stats.ffi_edges_created, 1);
    }

    // ---- F4: ALL endpoint parsing tests ----

    #[test]
    fn test_parse_endpoint_all_method() {
        let result = parse_endpoint_qualified_name("route::ALL::/health");
        assert_eq!(result, Some((HttpMethod::All, "/health".to_string())));
    }

    #[test]
    fn test_http_matching_all_method_endpoint() {
        use crate::graph::unified::build::helper::GraphBuildHelper;
        use crate::graph::unified::build::staging::StagingGraph;
        use crate::graph::unified::edge::kind::HttpMethod;
        use std::path::PathBuf;

        let mut graph = CodeGraph::new();

        // File 1: Server with ALL endpoint
        let server_path = PathBuf::from("/test/server.ts");
        {
            let mut staging = StagingGraph::new();
            let mut helper =
                GraphBuildHelper::new(&mut staging, &server_path, Language::TypeScript);
            let _endpoint = helper.add_endpoint("route::ALL::/health", None);
            commit_staging_to_graph(&mut graph, &server_path, Language::TypeScript, staging);
        }

        // File 2: Client making a specific GET request
        let client_path = PathBuf::from("/test/client.js");
        {
            let mut staging = StagingGraph::new();
            let mut helper =
                GraphBuildHelper::new(&mut staging, &client_path, Language::JavaScript);
            let fetch_fn = helper.add_function("checkHealth", None, true, false);
            let target = helper.add_module("http::/health", None);
            helper.add_http_request_edge(fetch_fn, target, HttpMethod::Get, Some("/health"));
            commit_staging_to_graph(&mut graph, &client_path, Language::JavaScript, staging);
        }

        let stats = link_cross_language_edges(&mut graph);
        // A specific GET request should match an ALL endpoint
        assert_eq!(stats.http_requests_scanned, 1);
        assert_eq!(stats.http_endpoints_matched, 1);
    }

    #[test]
    fn test_http_matching_all_method_request() {
        use crate::graph::unified::build::helper::GraphBuildHelper;
        use crate::graph::unified::build::staging::StagingGraph;
        use crate::graph::unified::edge::kind::HttpMethod;
        use std::path::PathBuf;

        let mut graph = CodeGraph::new();

        // File 1: Server with specific GET endpoint
        let server_path = PathBuf::from("/test/server.py");
        {
            let mut staging = StagingGraph::new();
            let mut helper = GraphBuildHelper::new(&mut staging, &server_path, Language::Python);
            let _endpoint = helper.add_endpoint("route::GET::/api/data", None);
            commit_staging_to_graph(&mut graph, &server_path, Language::Python, staging);
        }

        // File 2: Client making an ALL request (wildcard)
        let client_path = PathBuf::from("/test/client.js");
        {
            let mut staging = StagingGraph::new();
            let mut helper =
                GraphBuildHelper::new(&mut staging, &client_path, Language::JavaScript);
            let fetch_fn = helper.add_function("fetchData", None, true, false);
            let target = helper.add_module("http::/api/data", None);
            helper.add_http_request_edge(fetch_fn, target, HttpMethod::All, Some("/api/data"));
            commit_staging_to_graph(&mut graph, &client_path, Language::JavaScript, staging);
        }

        let stats = link_cross_language_edges(&mut graph);
        // An ALL request should match a specific GET endpoint
        assert_eq!(stats.http_requests_scanned, 1);
        assert_eq!(stats.http_endpoints_matched, 1);
    }

    // ---- F6: Typed URL params tests ----

    #[test]
    fn test_normalize_url_path_typed_params() {
        assert_eq!(normalize_url_path("/api/users/<int:id>"), "api/users/:id");
        assert_eq!(
            normalize_url_path("/files/<path:subpath>"),
            "files/:subpath"
        );
        // Regular angle bracket params still work
        assert_eq!(normalize_url_path("/api/users/<id>"), "api/users/:id");
    }

    // ==================================================================
    // Task 4 Step 4 Phase 2: rebuild-plane coverage.
    //
    // `link_cross_language_edges_generic` is the single generic
    // implementation behind the public `link_cross_language_edges`
    // shim. The test seeds an equivalent two-file FFI shape on a
    // `CodeGraph` and a `RebuildGraph`, runs the generic function
    // against each, and asserts the resulting edge counts match —
    // proving the trait-dispatched reads (`indices`, `nodes`,
    // `strings`, `files`, `edges`) and writes (`edges_mut`) all
    // route through `GraphMutationTarget` correctly on both
    // implementors.
    // ==================================================================

    #[test]
    #[cfg(feature = "rebuild-internals")]
    fn link_cross_language_edges_runs_against_rebuild_graph() {
        use super::link_cross_language_edges_generic;
        use crate::graph::unified::mutation_target::GraphMutationTarget;
        use crate::graph::unified::node::NodeKind;
        use crate::graph::unified::storage::NodeEntry;
        use std::path::Path;

        /// Seed: two files, one declares `extern::C::calc_sum` as a
        /// Rust-side FFI declaration, the other implements `calc_sum`
        /// as a C function. Pass 5 should link declaration → C impl
        /// with a cross-file `FfiCall` edge.
        fn seed<G: GraphMutationTarget>(graph: &mut G) -> (FileId, FileId) {
            let rust_file = graph
                .files_mut()
                .register_with_language(
                    Path::new("/virtual/ffi.rs"),
                    Some(crate::graph::node::Language::Rust),
                )
                .expect("register rust file");
            let c_file = graph
                .files_mut()
                .register_with_language(
                    Path::new("/virtual/ffi.c"),
                    Some(crate::graph::node::Language::C),
                )
                .expect("register c file");

            let decl_name = graph.strings_mut().intern("extern::C::calc_sum").unwrap();
            let impl_name = graph.strings_mut().intern("calc_sum").unwrap();

            let mut decl_entry = NodeEntry::new(NodeKind::Function, decl_name, rust_file);
            decl_entry.qualified_name = Some(decl_name);
            let _decl_id = graph.nodes_mut().alloc(decl_entry).unwrap();

            let mut impl_entry = NodeEntry::new(NodeKind::Function, impl_name, c_file);
            impl_entry.qualified_name = Some(impl_name);
            let _impl_id = graph.nodes_mut().alloc(impl_entry).unwrap();

            // Populate indices.by_kind(Function) and files bucket so
            // `collect_ffi_declarations` + `build_c_function_map` can
            // walk both sides.
            crate::graph::unified::build::parallel_commit::rebuild_indices(graph);

            (rust_file, c_file)
        }

        // Baseline: CodeGraph.
        let mut cg = CodeGraph::new();
        let (_cg_rust, _cg_c) = seed(&mut cg);
        let cg_stats = link_cross_language_edges_generic(&mut cg);

        // RebuildGraph path.
        let mut rebuild = {
            let graph = CodeGraph::new();
            graph.clone_for_rebuild()
        };
        let (_rb_rust, _rb_c) = seed(&mut rebuild);

        // Capture the rebuild-local forward edge count before Pass 5.
        let pre_counter = GraphMutationTarget::edges(&rebuild).forward().seq_counter();

        let rb_stats = link_cross_language_edges_generic(&mut rebuild);

        // === Stats parity ===
        assert_eq!(
            cg_stats.ffi_declarations_scanned,
            rb_stats.ffi_declarations_scanned
        );
        assert_eq!(cg_stats.ffi_edges_created, rb_stats.ffi_edges_created);
        assert_eq!(cg_stats.total_edges_created, rb_stats.total_edges_created);
        assert!(rb_stats.ffi_edges_created >= 1, "expected ≥1 FFI link");

        // === Invariant: the pending edges landed on the
        // rebuild-local forward store (not a CodeGraph). ===
        let forward = GraphMutationTarget::edges(&rebuild).forward();
        let after_counter = forward.seq_counter();
        assert!(
            after_counter > pre_counter,
            "rebuild-local forward store seq counter must advance \
             (pre={pre_counter} after={after_counter})",
        );
        let ffi_call_count = forward
            .delta()
            .iter()
            .filter(|e| e.is_add())
            .filter(|e| matches!(&e.kind, EdgeKind::FfiCall { .. }))
            .count();
        assert!(
            ffi_call_count >= 1,
            "rebuild-local forward delta must carry the FfiCall edge"
        );
    }
}
