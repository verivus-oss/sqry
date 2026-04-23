//! Per-plugin cross-file line-zero regression tests.
//!
//! For each of the 16 supported language plugins, builds a tiny 2-file corpus
//! where one file calls a function defined in the other, then verifies that
//! all call-compatible nodes in the resulting graph have `start_line > 0`.
//!
//! This is the per-plugin regression surface for the line-zero holistic fix.

use std::path::Path;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::node::NodeKind;
use sqry_core::plugin::PluginManager;

/// Call-compatible node kinds: should never have line 0 after unification
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
        .join("per_plugin_cross_file")
}

/// Build a graph from a per-plugin fixture directory using the specified plugin.
fn build_with_plugin(
    lang_dir: &str,
    plugin: Box<dyn sqry_core::plugin::LanguagePlugin>,
) -> CodeGraph {
    let path = fixtures_dir().join(lang_dir);
    assert!(path.exists(), "Fixture directory does not exist: {path:?}");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(plugin);
    let config = BuildConfig::default();
    build_unified_graph(&path, &plugins, &config)
        .unwrap_or_else(|e| panic!("Graph build failed for {lang_dir}: {e}"))
}

/// Assert the line-zero invariant: for call-compatible node groups, if ANY
/// member has a real span, ALL should. Stub-only groups (external references
/// like `println!`) are acceptable at line 0.
fn assert_no_line_zero(graph: &CodeGraph, lang: &str) {
    let files = graph.files();
    let strings = graph.strings();
    let mut call_compatible_count = 0;

    // Group call-compatible nodes by qualified name
    let mut groups: std::collections::HashMap<
        String,
        Vec<(sqry_core::graph::unified::node::NodeId, u32, bool)>,
    > = std::collections::HashMap::new();

    for (node_id, entry) in graph.nodes().iter() {
        if !CALL_COMPATIBLE_KINDS.contains(&entry.kind) {
            continue;
        }
        call_compatible_count += 1;

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

    // Sanity: we should have found at least one call-compatible node
    assert!(
        call_compatible_count > 0,
        "[{lang}] No call-compatible nodes found — fixture may be malformed"
    );

    // For each group: if any member has a real span, all must
    for (qn, members) in &groups {
        let has_real_span = members.iter().any(|(_, line, _)| *line > 0);
        if !has_real_span {
            continue; // Stub-only group (external reference) — acceptable
        }
        for (node_id, line, is_external) in members {
            if *line > 0 || *is_external {
                continue;
            }
            panic!(
                "[{lang}] Node {node_id:?} ({qn}) has start_line == 0 but a sibling has a \
                 real span — cross-file unification should have fixed this"
            );
        }
    }
}

/// Assert the graph has at least one node (basic sanity).
fn assert_nonempty_graph(graph: &CodeGraph, lang: &str) {
    assert!(
        graph.node_count() > 0,
        "[{lang}] Graph has no nodes — plugin may not support this fixture"
    );
}

/// Assert that a caller node exists, has at least one callee via Calls edges,
/// and that the expected cross-file callee has line > 0. This is a strict check:
/// if the caller or callee is missing, the test FAILS (not skips).
fn assert_cross_file_callee_resolved(
    graph: &CodeGraph,
    lang: &str,
    caller_name_fragment: &str,
    callee_name_fragment: &str,
) {
    let strings = graph.strings();

    // Find the caller — must exist
    let caller_id = graph.nodes().iter().find_map(|(id, entry)| {
        if matches!(entry.kind, NodeKind::Function | NodeKind::Method)
            && entry.start_line > 0
            && strings
                .resolve(entry.name)
                .is_some_and(|n| n.contains(caller_name_fragment))
        {
            Some(id)
        } else {
            None
        }
    });

    assert!(
        caller_id.is_some(),
        "[{lang}] Caller '{caller_name_fragment}' not found in graph — fixture is malformed"
    );
    let caller_id = caller_id.unwrap();

    // Collect callees via Calls edges — must have at least one
    let mut callees: Vec<(String, u32)> = Vec::new();
    for edge_ref in graph.edges().edges_from(caller_id) {
        if matches!(
            edge_ref.kind,
            sqry_core::graph::unified::edge::EdgeKind::Calls { .. }
        ) && let Some(target) = graph.nodes().get(edge_ref.target)
        {
            let resolved = strings.resolve(target.name);
            let name = resolved.as_deref().unwrap_or("").to_string();
            callees.push((name, target.start_line));
        }
    }

    assert!(
        !callees.is_empty(),
        "[{lang}] Caller '{caller_name_fragment}' has no Calls edges — \
         cross-file call edge emission is broken"
    );

    // The expected callee must exist among the callees with line > 0
    let callee_names: Vec<&str> = callees.iter().map(|(n, _)| n.as_str()).collect();
    let matching_callee = callees
        .iter()
        .find(|(n, _)| n.contains(callee_name_fragment));
    assert!(
        matching_callee.is_some(),
        "[{lang}] Expected callee '{callee_name_fragment}' not found in Calls edges of \
         '{caller_name_fragment}'. Found: {callee_names:?}"
    );
    let (name, line) = matching_callee.unwrap();
    assert_ne!(
        *line, 0,
        "[{lang}] Cross-file callee {name} has line == 0 — resolution failed"
    );
}

// ---------------------------------------------------------------------------
// Per-plugin sub-tests
// ---------------------------------------------------------------------------

#[test]
fn cross_file_c() {
    let graph = build_with_plugin("c", Box::new(sqry_lang_c::CPlugin::new()));
    assert_nonempty_graph(&graph, "c");
    assert_no_line_zero(&graph, "c");
    assert_cross_file_callee_resolved(&graph, "c", "process_data", "compute_value");
}

#[test]
fn cross_file_cpp() {
    let graph = build_with_plugin("cpp", Box::new(sqry_lang_cpp::CppPlugin::new()));
    assert_nonempty_graph(&graph, "cpp");
    assert_no_line_zero(&graph, "cpp");
    assert_cross_file_callee_resolved(&graph, "cpp", "run_pipeline", "transform_data");
}

#[test]
fn cross_file_rust() {
    let graph = build_with_plugin("rust", Box::new(sqry_lang_rust::RustPlugin::default()));
    assert_nonempty_graph(&graph, "rust");
    assert_no_line_zero(&graph, "rust");
    assert_cross_file_callee_resolved(&graph, "rust", "orchestrate", "compute");
}

#[test]
fn cross_file_go() {
    let graph = build_with_plugin("go", Box::new(sqry_lang_go::GoPlugin::new()));
    assert_nonempty_graph(&graph, "go");
    assert_no_line_zero(&graph, "go");
    assert_cross_file_callee_resolved(&graph, "go", "ProcessData", "ComputeValue");
}

#[test]
fn cross_file_java() {
    let graph = build_with_plugin("java", Box::new(sqry_lang_java::JavaPlugin::new()));
    assert_nonempty_graph(&graph, "java");
    assert_no_line_zero(&graph, "java");
}

#[test]
fn cross_file_python() {
    let graph = build_with_plugin("python", Box::new(sqry_lang_python::PythonPlugin::new()));
    assert_nonempty_graph(&graph, "python");
    assert_no_line_zero(&graph, "python");
    assert_cross_file_callee_resolved(&graph, "python", "process_data", "compute_value");
}

#[test]
fn cross_file_typescript() {
    let graph = build_with_plugin(
        "typescript",
        Box::new(sqry_lang_typescript::TypeScriptPlugin::new()),
    );
    assert_nonempty_graph(&graph, "typescript");
    assert_no_line_zero(&graph, "typescript");
    assert_cross_file_callee_resolved(&graph, "typescript", "processData", "computeValue");
}

#[test]
fn cross_file_javascript() {
    let graph = build_with_plugin(
        "javascript",
        Box::new(sqry_lang_javascript::JavaScriptPlugin::new()),
    );
    assert_nonempty_graph(&graph, "javascript");
    assert_no_line_zero(&graph, "javascript");
    assert_cross_file_callee_resolved(&graph, "javascript", "processData", "computeValue");
}

#[test]
fn cross_file_kotlin() {
    let graph = build_with_plugin("kotlin", Box::new(sqry_lang_kotlin::KotlinPlugin::new()));
    assert_nonempty_graph(&graph, "kotlin");
    assert_no_line_zero(&graph, "kotlin");
}

#[test]
fn cross_file_scala() {
    let graph = build_with_plugin("scala", Box::new(sqry_lang_scala::ScalaPlugin::new()));
    assert_nonempty_graph(&graph, "scala");
    assert_no_line_zero(&graph, "scala");
}

#[test]
fn cross_file_ruby() {
    let graph = build_with_plugin("ruby", Box::new(sqry_lang_ruby::RubyPlugin::new()));
    assert_nonempty_graph(&graph, "ruby");
    assert_no_line_zero(&graph, "ruby");
}

#[test]
fn cross_file_php() {
    let graph = build_with_plugin("php", Box::new(sqry_lang_php::PhpPlugin::new()));
    assert_nonempty_graph(&graph, "php");
    assert_no_line_zero(&graph, "php");
    assert_cross_file_callee_resolved(&graph, "php", "processData", "computeValue");
}

#[test]
fn cross_file_swift() {
    let graph = build_with_plugin("swift", Box::new(sqry_lang_swift::SwiftPlugin::new()));
    assert_nonempty_graph(&graph, "swift");
    assert_no_line_zero(&graph, "swift");
}

#[test]
fn cross_file_dart() {
    let graph = build_with_plugin("dart", Box::new(sqry_lang_dart::DartPlugin::new()));
    assert_nonempty_graph(&graph, "dart");
    assert_no_line_zero(&graph, "dart");
}

#[test]
fn cross_file_lua() {
    let graph = build_with_plugin("lua", Box::new(sqry_lang_lua::LuaPlugin::new()));
    assert_nonempty_graph(&graph, "lua");
    assert_no_line_zero(&graph, "lua");
}

#[test]
fn cross_file_r() {
    let graph = build_with_plugin("r", Box::new(sqry_lang_r::RPlugin::new()));
    assert_nonempty_graph(&graph, "r");
    assert_no_line_zero(&graph, "r");
    assert_cross_file_callee_resolved(&graph, "r", "process_data", "compute_value");
}
