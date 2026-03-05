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
use crate::graph::unified::node::{NodeId, NodeKind};

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
    /// Edge kind (FfiCall or HttpRequest).
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
pub fn link_cross_language_edges(graph: &mut CodeGraph) -> Pass5Stats {
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
    /// The bare function name to match against (e.g., "puts" from "extern::C::puts").
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
                "C" => FfiConvention::C,
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
    {
        // Python ctypes/cffi style
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
    format!("Java_{}", java_name.replace('.', "_"))
}

/// Collect FFI declarations from the graph and match them to C/C++ implementations.
fn link_ffi_edges(
    graph: &CodeGraph,
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
        if !matched && decl.bare_name.contains('.') {
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
        if !matched
            && decl.bare_name.contains('.')
            && let Some(method_name) = decl.bare_name.rsplit('.').next()
        {
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

/// Collect FFI declaration nodes from the graph.
fn collect_ffi_declarations(graph: &CodeGraph, stats: &mut Pass5Stats) -> Vec<FfiDeclaration> {
    let mut declarations = Vec::new();
    let function_nodes = graph.indices().by_kind(NodeKind::Function);

    for &node_id in function_nodes {
        let Some(entry) = graph.nodes().get(node_id) else {
            continue;
        };

        let Some(name_str) = graph.strings().resolve(entry.name) else {
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
fn build_c_function_map(graph: &CodeGraph) -> HashMap<String, Vec<(NodeId, FileId)>> {
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
                let Some(name_str) = graph.strings().resolve(entry.name) else {
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
fn link_http_edges(
    graph: &CodeGraph,
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
fn collect_endpoints(graph: &CodeGraph) -> Vec<EndpointInfo> {
    let mut endpoints = Vec::new();
    let endpoint_nodes = graph.indices().by_kind(NodeKind::Endpoint);

    for &node_id in endpoint_nodes {
        let Some(entry) = graph.nodes().get(node_id) else {
            continue;
        };
        let Some(name_str) = graph.strings().resolve(entry.name) else {
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

/// Collect all HTTP request edges from the graph.
fn collect_http_requests(graph: &CodeGraph, stats: &mut Pass5Stats) -> Vec<HttpRequestInfo> {
    let mut requests = Vec::new();

    // Iterate all nodes and check their outgoing edges for HttpRequest edges
    let all_nodes = graph.nodes();
    for idx in 0..all_nodes.capacity() {
        let node_id = NodeId::new(idx as u32, 0);
        // Try to get edges from this node - edges_from handles generation correctly
        let edges = graph.edges().edges_from(node_id);
        for edge in edges {
            if let EdgeKind::HttpRequest { method, url } = &edge.kind {
                stats.http_requests_scanned += 1;
                // Resolve the URL string
                if let Some(url_id) = url
                    && let Some(url_str) = graph.strings().resolve(*url_id)
                {
                    let Some(source_entry) = graph.nodes().get(edge.source) else {
                        continue;
                    };
                    requests.push(HttpRequestInfo {
                        source_node: edge.source,
                        method: *method,
                        url_path: url_str.to_string(),
                        file_id: source_entry.file,
                    });
                }
            }
        }
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

    /// Helper to commit a staging graph to the main CodeGraph.
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
}
