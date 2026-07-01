//! End-to-end line-zero regression tests.
//!
//! Verifies that the holistic line-zero fix (HU01-HU07) eliminates line-0
//! reports across cross-file builds. Tests exercise:
//!
//! - **Kernel-style C fixture**: macro calls + cross-file function calls
//! - **Build-time arena invariant**: no call-compatible node has line 0
//!   unless it is in an external file (classpath/header boundary)
//! - **Determinism**: repeated builds of the same fixture produce identical
//!   unification stats and arena SHA-256 hashes

use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::PluginManager;
use sqry_lang_c::CPlugin;
use sqry_lang_cpp::CppPlugin;

/// Call-compatible node kinds that should never have line 0 after unification,
/// unless the node lives in an external file.
const CALL_COMPATIBLE_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Macro,
    NodeKind::Constant,
    NodeKind::LambdaTarget,
];

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn build_c_fixture(fixture_name: &str) -> CodeGraph {
    let path = fixtures_dir().join(fixture_name);
    assert!(path.exists(), "Fixture directory does not exist: {path:?}");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(CPlugin::new()));
    plugins.register_builtin(Box::new(CppPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(&path, &plugins, &config).expect("graph build should succeed")
}

/// Compute a deterministic hash of all arena nodes for comparison.
fn arena_hash(graph: &CodeGraph) -> String {
    let mut hasher = Sha256::new();
    let strings = graph.strings();
    for (_id, entry) in graph.nodes().iter() {
        let resolved = strings.resolve(entry.name);
        let name = resolved.as_deref().unwrap_or("<unresolved>");
        hasher.update(name.as_bytes());
        hasher.update(format!("{:?}", entry.kind).as_bytes());
        hasher.update(entry.start_line.to_le_bytes());
        hasher.update(entry.start_column.to_le_bytes());
        hasher.update(entry.file.index().to_le_bytes());
    }
    hex::encode(hasher.finalize())
}

/// Assert the build-time arena invariant: for every call-compatible node group
/// (grouped by qualified name), if ANY member has `start_line > 0`, then ALL
/// members must have `start_line > 0` (Phase 4c-prime should have unified stubs).
///
/// Stubs that reference external symbols (no workspace definition at all) are
/// acceptable at line 0 — they represent `println!`, `kfree`, etc. from outside
/// the workspace. The `is_external` flag covers classpath/header boundary cases;
/// stub-only groups (no sibling with a real span) cover standard library references.
fn assert_no_line_zero_in_call_compatible(graph: &CodeGraph, context: &str) {
    let strings = graph.strings();
    let files = graph.files();

    // Group call-compatible nodes by qualified name (or plain name as fallback)
    let mut groups: HashMap<String, Vec<(sqry_core::graph::unified::node::NodeId, u32, bool)>> =
        HashMap::new();
    for (node_id, entry) in graph.nodes().iter() {
        if !CALL_COMPATIBLE_KINDS.contains(&entry.kind) {
            continue;
        }
        let key = entry
            .qualified_name
            .and_then(|id| strings.resolve(id))
            .or_else(|| strings.resolve(entry.name))
            .map(|s| s.to_string())
            .unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let is_external = files.is_external(entry.file);
        groups
            .entry(key)
            .or_default()
            .push((node_id, entry.start_line, is_external));
    }

    // For each group: if any member has a real span, all must have a real span
    for (qn, members) in &groups {
        let has_real_span = members.iter().any(|(_, line, _)| *line > 0);
        if !has_real_span {
            // Stub-only group: all members are external references — acceptable
            continue;
        }
        for (node_id, line, is_external) in members {
            if *line > 0 || *is_external {
                continue;
            }
            panic!(
                "[{context}] Node {node_id:?} ({qn}, kind=call-compatible) has start_line == 0 \
                 but a sibling has a real span — Phase 4c-prime should have unified this"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Kernel-style C fixture tests
// ---------------------------------------------------------------------------

/// After building the kernel fixture, `nft_add_set_elem`'s callees must all
/// have `line > 0`. This is the core regression for the 2026-04-11 Linux audit.
#[test]
fn kernel_fixture_callees_have_nonzero_lines() {
    let graph = build_c_fixture("cross_file_c_kernel");
    let strings = graph.strings();

    // Find nft_add_set_elem
    let caller_node = graph
        .nodes()
        .iter()
        .find(|(_, entry)| {
            strings
                .resolve(entry.name)
                .is_some_and(|n| n.contains("nft_add_set_elem"))
                && matches!(entry.kind, NodeKind::Function)
        })
        .map(|(id, _)| id);

    assert!(
        caller_node.is_some(),
        "nft_add_set_elem should exist in the kernel fixture graph"
    );
    let caller_id = caller_node.unwrap();

    // Collect all callees via Calls edges
    let mut callees: Vec<(String, NodeKind, u32)> = Vec::new();
    for edge_ref in graph.edges().edges_from(caller_id) {
        if matches!(
            edge_ref.kind,
            sqry_core::graph::unified::edge::EdgeKind::Calls { .. }
        ) && let Some(target_entry) = graph.nodes().get(edge_ref.target)
        {
            let resolved = strings.resolve(target_entry.name);
            let name = resolved.as_deref().unwrap_or("<unresolved>").to_string();
            callees.push((name, target_entry.kind, target_entry.start_line));
        }
    }

    assert!(
        !callees.is_empty(),
        "nft_add_set_elem should have at least one callee"
    );

    // Every callee must have line > 0
    for (name, kind, line) in &callees {
        assert_ne!(
            *line, 0,
            "Callee {name} (kind={kind:?}) of nft_add_set_elem has line == 0: \
             the holistic fix should have resolved this"
        );
    }

    // Verify specific expected callees exist (HU01/HU02 generalization coverage)
    let callee_names: Vec<&str> = callees.iter().map(|(n, _, _)| n.as_str()).collect();

    // Cross-file function callee
    assert!(
        callee_names
            .iter()
            .any(|n| n.contains("nft_register_chain")),
        "nft_add_set_elem should call nft_register_chain (cross-file function). \
         Found callees: {callee_names:?}"
    );

    // Macro callees from nft_fake_macros.h — verifies CALL_COMPATIBLE_KINDS
    // generalization handles Macro kind correctly
    for expected_macro in &["list_for_each_entry", "kfree", "nft_set_ext_exists"] {
        assert!(
            callee_names.iter().any(|n| n.contains(expected_macro)),
            "nft_add_set_elem should call {expected_macro} (macro from nft_fake_macros.h). \
             Found callees: {callee_names:?}"
        );
    }

    // Verify macro-originating callees have line > 0 and a call-compatible kind.
    // Note: C macros may be parsed as Function or Macro depending on how
    // tree-sitter sees the call site (e.g., `kfree(ptr)` looks like a function call).
    for (name, kind, line) in &callees {
        if ["list_for_each_entry", "kfree", "nft_set_ext_exists"]
            .iter()
            .any(|m| name.contains(m))
        {
            assert!(
                matches!(
                    kind,
                    NodeKind::Macro | NodeKind::Function | NodeKind::Method
                ),
                "Macro-originating callee {name} should have a call-compatible kind, got {kind:?}"
            );
            assert!(
                *line > 0,
                "Macro-originating callee {name} should have line > 0, got {line}"
            );
        }
    }
}

/// The kernel fixture's nft_register_chain (in nft_fake_helpers.c) should
/// have callers reachable via reverse edges, and those callers must have line > 0.
#[test]
fn kernel_fixture_cross_file_callers_have_nonzero_lines() {
    let graph = build_c_fixture("cross_file_c_kernel");
    let strings = graph.strings();

    // Find nft_register_chain (the cross-file target)
    let helper_id = graph
        .nodes()
        .iter()
        .find(|(_, entry)| {
            strings
                .resolve(entry.name)
                .is_some_and(|n| n.contains("nft_register_chain"))
                && matches!(entry.kind, NodeKind::Function)
                && entry.start_line > 0
        })
        .map(|(id, _)| id);

    assert!(
        helper_id.is_some(),
        "nft_register_chain should exist in the kernel fixture graph with line > 0"
    );
    let helper_id = helper_id.unwrap();

    // Validate reverse edges: collect callers of nft_register_chain
    let mut callers: Vec<(String, u32)> = Vec::new();
    for edge_ref in graph.edges().edges_to(helper_id) {
        if matches!(
            edge_ref.kind,
            sqry_core::graph::unified::edge::EdgeKind::Calls { .. }
        ) && let Some(caller_entry) = graph.nodes().get(edge_ref.source)
        {
            let resolved = strings.resolve(caller_entry.name);
            let name = resolved.as_deref().unwrap_or("<unresolved>").to_string();
            callers.push((name, caller_entry.start_line));
        }
    }

    assert!(
        !callers.is_empty(),
        "nft_register_chain should have at least one caller via reverse edges"
    );

    // All callers must have line > 0
    for (name, line) in &callers {
        assert_ne!(
            *line, 0,
            "Caller {name} of nft_register_chain has line == 0 — \
             reverse-edge caller reporting is broken"
        );
    }

    // nft_add_set_elem should be among the callers
    assert!(
        callers.iter().any(|(n, _)| n.contains("nft_add_set_elem")),
        "nft_add_set_elem should appear as a caller of nft_register_chain. \
         Found callers: {callers:?}"
    );
}

/// Build-time arena invariant: no call-compatible node in the kernel fixture
/// has line 0 (unless external).
#[test]
fn kernel_fixture_arena_invariant_no_line_zero() {
    let graph = build_c_fixture("cross_file_c_kernel");
    assert_no_line_zero_in_call_compatible(&graph, "cross_file_c_kernel");
}

// ---------------------------------------------------------------------------
// Determinism tests
// ---------------------------------------------------------------------------

/// The kernel fixture must produce identical arena hashes across 10 builds.
#[test]
fn kernel_fixture_determinism_10_builds() {
    let mut hashes = Vec::new();
    let mut node_counts = Vec::new();

    for _ in 0..10 {
        let graph = build_c_fixture("cross_file_c_kernel");
        hashes.push(arena_hash(&graph));
        node_counts.push(graph.node_count());
    }

    let first_hash = &hashes[0];
    for (i, hash) in hashes.iter().enumerate().skip(1) {
        assert_eq!(
            first_hash, hash,
            "Build {i} produced different arena hash than build 0"
        );
    }

    let first_count = node_counts[0];
    for (i, count) in node_counts.iter().enumerate().skip(1) {
        assert_eq!(
            first_count, *count,
            "Build {i} produced different node count ({count}) than build 0 ({first_count})"
        );
    }
}

/// dec44131f's TypeScript cross-kind regression (Method<->Function) still
/// passes after the HU01 generalization.
#[test]
fn typescript_cross_kind_regression_still_passes() {
    let ts_fixture = fixtures_dir()
        .join("per_plugin_cross_file")
        .join("typescript");
    if !ts_fixture.exists() {
        panic!("TypeScript fixture should exist: {ts_fixture:?}");
    }

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(sqry_lang_typescript::TypeScriptPlugin::new()));
    let config = BuildConfig::default();
    let graph = build_unified_graph(&ts_fixture, &plugins, &config)
        .expect("TypeScript fixture build should succeed");

    assert_no_line_zero_in_call_compatible(&graph, "typescript cross-kind regression");
}

// ---------------------------------------------------------------------------
// Multi-language fixture tests
// ---------------------------------------------------------------------------

/// Multi-language fixture: Rust→C FFI edge should have resolved target with
/// line > 0 (when cross-language linking succeeds).
#[test]
fn multi_language_fixture_arena_invariant() {
    let path = fixtures_dir().join("cross_file_multi_lang");
    assert!(
        path.exists(),
        "Multi-language fixture should exist: {path:?}"
    );

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(CPlugin::new()));
    plugins.register_builtin(Box::new(sqry_lang_rust::RustPlugin::default()));
    plugins.register_builtin(Box::new(sqry_lang_javascript::JavaScriptPlugin::new()));
    plugins.register_builtin(Box::new(sqry_lang_python::PythonPlugin::new()));
    let config = BuildConfig::default();
    let graph = build_unified_graph(&path, &plugins, &config)
        .expect("Multi-language fixture build should succeed");

    // Arena invariant: no line-0 call-compatible nodes in non-external files
    assert_no_line_zero_in_call_compatible(&graph, "cross_file_multi_lang");

    // Verify we actually indexed files from multiple languages
    let mut lang_files: HashMap<String, usize> = HashMap::new();
    for (_file_id, file_path) in graph.files().iter() {
        let ext = file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        *lang_files.entry(ext).or_default() += 1;
    }
    assert!(
        lang_files.len() >= 3,
        "Multi-language fixture should index files from at least 3 languages, \
         got extensions: {lang_files:?}"
    );

    let strings = graph.strings();

    // Verify Rust extern "C" { fn c_helper(...) } creates an FFI-related node.
    // The Rust plugin should create a Function/Method node for extern declarations.
    let has_c_helper = graph.nodes().iter().any(|(_, entry)| {
        strings
            .resolve(entry.name)
            .is_some_and(|n| n.contains("c_helper"))
    });
    assert!(
        has_c_helper,
        "Multi-language fixture should contain c_helper node (Rust extern 'C' declaration)"
    );

    // Unconditionally verify that any FfiCall/HttpRequest edge target has line > 0.
    // Pass 5 links FFI declarations → C functions and HTTP requests → endpoints.
    for (src_id, _) in graph.nodes().iter() {
        for edge_ref in graph.edges().edges_from(src_id) {
            if matches!(
                &edge_ref.kind,
                sqry_core::graph::unified::edge::EdgeKind::FfiCall { .. }
                    | sqry_core::graph::unified::edge::EdgeKind::HttpRequest { .. }
            ) && let Some(target) = graph.nodes().get(edge_ref.target)
            {
                let target_name = strings
                    .resolve(target.name)
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                assert_ne!(
                    target.start_line, 0,
                    "Cross-language edge target {target_name} ({:?}) has line == 0",
                    target.kind
                );
            }
        }
    }

    // Verify BOTH sides of the multi-language fixture are indexed:
    // JS side: fetchUsers/createUser from webclient.js
    let has_js_function = graph.nodes().iter().any(|(_, entry)| {
        strings
            .resolve(entry.name)
            .is_some_and(|n| n.contains("fetchUsers") || n.contains("createUser"))
    });
    assert!(
        has_js_function,
        "Multi-language fixture should contain fetchUsers/createUser from webclient.js"
    );

    // Python side: get_users/create_user from webserver.py
    let has_py_function = graph.nodes().iter().any(|(_, entry)| {
        strings
            .resolve(entry.name)
            .is_some_and(|n| n.contains("get_users") || n.contains("create_user"))
    });
    assert!(
        has_py_function,
        "Multi-language fixture should contain get_users/create_user from webserver.py"
    );

    // C side: c_helper definition from callee.c (already checked above)
    // Rust side: call_native from caller.rs
    let has_rust_function = graph.nodes().iter().any(|(_, entry)| {
        strings
            .resolve(entry.name)
            .is_some_and(|n| n.contains("call_native"))
    });
    assert!(
        has_rust_function,
        "Multi-language fixture should contain call_native from caller.rs"
    );
}
