use super::*;
use crate::graph::node::Language;
use crate::graph::unified::concurrent::CodeGraph;
use crate::graph::unified::node::NodeKind;
use crate::graph::unified::storage::arena::NodeEntry;
use std::path::Path;
use std::sync::Arc;

#[test]
fn test_executor_new() {
    let executor = QueryExecutor::new();
    // Executor is created successfully (plugins will be registered at runtime)
    assert!(executor.plugin_manager.plugins().is_empty());
}

#[test]
fn test_execute_on_graph_nonexistent_path() {
    let executor = QueryExecutor::new();
    let result = executor.execute_on_graph("kind:function", Path::new("/nonexistent/path"));
    // Should error because no graph exists
    assert!(result.is_err());
}

// Note: Full integration tests with actual plugins are in the CLI integration tests
// where plugins are properly registered. Tests for predicate evaluation and index
// operations were removed as part of the legacy index to CodeGraph migration.

// ------------------------------------------------------------------------
// Shared fixture for `execute_on_preloaded_graph` tests.
// ------------------------------------------------------------------------
//
// Builds a small `CodeGraph` with two Function nodes and one Struct node,
// all registered in the auxiliary indices so `kind:function` predicate
// evaluation returns a non-empty result. Mirrors the pattern in
// `sqry-core/tests/unified_graph_indices_persistence_test.rs::create_test_graph`
// but minimized to the smallest set that exercises the preloaded-graph
// code path end-to-end.
fn build_small_preloaded_graph() -> CodeGraph {
    let mut graph = CodeGraph::new();

    let file_id = graph
        .files_mut()
        .register_with_language(Path::new("/test/lib.rs"), Some(Language::Rust))
        .expect("register file");

    let nodes: &[(&str, &str, NodeKind, u32, u32)] = &[
        ("alpha", "test::alpha", NodeKind::Function, 1, 10),
        ("beta", "test::beta", NodeKind::Function, 12, 20),
        ("Holder", "test::Holder", NodeKind::Struct, 22, 30),
    ];

    for (name, qname, kind, start_line, end_line) in nodes {
        let name_id = graph.strings_mut().intern(name).expect("intern name");
        let qname_id = graph.strings_mut().intern(qname).expect("intern qname");

        let entry = NodeEntry::new(*kind, name_id, file_id)
            .with_location(*start_line, 0, *end_line, 0)
            .with_qualified_name(qname_id);

        let node_id = graph.nodes_mut().alloc(entry.clone()).expect("alloc node");

        graph.indices_mut().add(
            node_id,
            entry.kind,
            entry.name,
            entry.qualified_name,
            entry.file,
        );
    }

    graph
}

#[test]
fn execute_on_preloaded_graph_success() {
    let executor = QueryExecutor::new();
    let graph = Arc::new(build_small_preloaded_graph());
    let workspace_root = Path::new("/test");

    let results = executor
        .execute_on_preloaded_graph(Arc::clone(&graph), "kind:function", workspace_root, None)
        .expect("execute_on_preloaded_graph succeeds");

    // Fixture has two Function nodes.
    assert_eq!(
        results.len(),
        2,
        "expected the two Function nodes to match kind:function"
    );

    // Collect the matched names for assertion on set identity.
    let mut names: Vec<String> = results
        .iter()
        .map(|m| m.name().map(|n| n.to_string()).unwrap_or_default())
        .collect();
    names.sort();
    assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
}

#[test]
fn execute_on_preloaded_graph_bypasses_cache() {
    let executor = QueryExecutor::new();

    // Pre-condition: no graph in the executor's process-wide cache.
    assert!(
        executor.graph_cache.read().is_none(),
        "executor graph_cache should start empty"
    );

    let graph = Arc::new(build_small_preloaded_graph());
    let workspace_root = Path::new("/test");

    let _results = executor
        .execute_on_preloaded_graph(Arc::clone(&graph), "kind:function", workspace_root, None)
        .expect("execute_on_preloaded_graph succeeds");

    // Post-condition: the preloaded-graph path must NOT have written into
    // the executor's graph cache. This is the core daemon invariant — the
    // daemon holds the graph under a workspace lock and must not let the
    // executor pollute its process-wide cache with a stale reference.
    assert!(
        executor.graph_cache.read().is_none(),
        "execute_on_preloaded_graph must not populate graph_cache"
    );
}

#[test]
fn execute_on_preloaded_graph_identity() {
    let executor = QueryExecutor::new();
    let graph_arc = Arc::new(build_small_preloaded_graph());
    let workspace_root = Path::new("/test");

    let results = executor
        .execute_on_preloaded_graph(
            Arc::clone(&graph_arc),
            "kind:function",
            workspace_root,
            None,
        )
        .expect("execute_on_preloaded_graph succeeds");

    // The QueryResults constructor (results.rs line 46) takes the same
    // Arc<CodeGraph> we passed in, so the results' underlying graph must
    // be pointer-equal to the one we supplied. This guarantees the
    // preloaded-graph path is genuinely zero-copy at the graph level.
    assert!(
        Arc::ptr_eq(&graph_arc, results.graph_arc_for_test()),
        "QueryResults graph Arc must be pointer-equal to the caller-supplied Arc"
    );
}

// ---------------------------------------------------------------------------
// T38 (surface parity W1 round 3, design D18, codex Finding 5): the executor
// builds only through an injected hook. Both tests require the
// `SQRY_AUTO_INDEX` gate to be on (unset, or not `false`/`0`): with the
// gate off both branches answer "no build" on every head and the tests
// would pass without observing anything, so the gate state is asserted
// first and a run with it off fails naming that, never passes vacuously.
// ---------------------------------------------------------------------------

fn assert_auto_index_gate_is_on() {
    let gate = std::env::var("SQRY_AUTO_INDEX").unwrap_or_default();
    assert!(
        gate != "false" && gate != "0",
        "SQRY_AUTO_INDEX={gate:?} turns the build branch off; these tests observe nothing then"
    );
}

/// A source tree the registered builder can build: two functions in
/// `src/lib.rs`. A build that ran here would succeed and write an index.
fn write_buildable_source(root: &Path) {
    std::fs::create_dir_all(root.join("src")).expect("src dir");
    std::fs::write(
        root.join("src").join("lib.rs"),
        b"pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 2 }\n",
    )
    .expect("write lib.rs");
}

/// A plugin this crate's own tests can register: it parses `.rs` files
/// with the Rust grammar and its builder records two function nodes per
/// file, so a build over `src/lib.rs` succeeds and persists.
/// `sqry_lang_rust::RustPlugin` implements the dependency crate's
/// `LanguagePlugin`, not the lib-under-test's `crate::plugin::LanguagePlugin`,
/// so the integration-test pattern (`sqry-core/tests/ast_query_tests.rs`)
/// does not carry into a unit test; this double is the in-crate form of
/// the same precondition (design D22).
struct BuildingRustPlugin;

impl crate::plugin::LanguagePlugin for BuildingRustPlugin {
    fn metadata(&self) -> crate::plugin::LanguageMetadata {
        crate::plugin::LanguageMetadata {
            id: "rust",
            name: "Rust",
            version: "test",
            author: "sqry-core tests",
            description: "Test-only Rust plugin whose builder records two functions per file",
            tree_sitter_version: "0.25",
        }
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse_ast(
        &self,
        content: &[u8],
    ) -> Result<tree_sitter::Tree, crate::plugin::error::ParseError> {
        use crate::plugin::error::ParseError;

        let mut parser = tree_sitter::Parser::new();
        let language = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(|err| ParseError::LanguageSetFailed(err.to_string()))?;
        parser
            .parse(content, None)
            .ok_or(ParseError::TreeSitterFailed)
    }

    fn extract_scopes(
        &self,
        _tree: &tree_sitter::Tree,
        _content: &[u8],
        _file_path: &Path,
    ) -> Result<Vec<crate::ast::Scope>, crate::plugin::error::ScopeError> {
        Ok(Vec::new())
    }

    fn graph_builder(&self) -> Option<&dyn crate::graph::GraphBuilder> {
        Some(&TwoFunctionsPerFileBuilder)
    }
}

/// The builder behind [`BuildingRustPlugin`]: two function nodes per file.
struct TwoFunctionsPerFileBuilder;

impl crate::graph::GraphBuilder for TwoFunctionsPerFileBuilder {
    fn build_graph(
        &self,
        _tree: &tree_sitter::Tree,
        _content: &[u8],
        file: &Path,
        staging: &mut crate::graph::unified::build::staging::StagingGraph,
    ) -> crate::graph::GraphResult<()> {
        use crate::graph::unified::build::helper::GraphBuildHelper;

        let mut helper = GraphBuildHelper::new(staging, file, Language::Rust);
        let alpha = helper.add_function("alpha", None, false, false);
        let beta = helper.add_function("beta", None, false, false);
        helper.add_call_edge(alpha, beta);
        Ok(())
    }

    fn language(&self) -> Language {
        Language::Rust
    }
}

/// A manager that would build if a build were attempted (surface parity
/// W1 round 4, design D22), and the measurement that it would: an
/// in-memory build of the fixture at `root` through this manager yields
/// a graph with the fixture's functions. With an empty manager a restored
/// direct build fails inside the build before any write, and the
/// no-build assertions below would accept it.
fn manager_that_can_build(root: &Path) -> crate::plugin::PluginManager {
    let mut manager = crate::plugin::PluginManager::new();
    manager.register_builtin(Box::new(BuildingRustPlugin));
    assert!(
        manager.plugin_by_id("rust").is_some(),
        "fixture precondition: the rust plugin is registered"
    );
    let built = crate::graph::unified::build::build_unified_graph(
        root,
        &manager,
        &crate::graph::unified::build::BuildConfig::default(),
    )
    .expect("fixture precondition: the manager builds the fixture in memory");
    assert!(
        built.node_count() >= 2,
        "fixture precondition: a build through this manager records the fixture's functions; \
         got {} nodes",
        built.node_count()
    );
    manager
}

/// Write a manifest beside a snapshot that cannot load, so
/// `get_or_load_graph` reaches its load-failure branch. Returns the
/// snapshot bytes and the manifest bytes as written.
fn write_corrupt_index(root: &Path) -> (Vec<u8>, Vec<u8>) {
    use crate::graph::unified::persistence::{BuildProvenance, GraphStorage, Manifest};

    let storage = GraphStorage::new(root);
    std::fs::create_dir_all(storage.graph_dir()).expect("graph dir");
    Manifest::new(
        root.to_string_lossy().to_string(),
        1,
        1,
        "fixture-sha256",
        BuildProvenance::new("test", "test:t38"),
    )
    .save(storage.manifest_path())
    .expect("manifest saved");
    std::fs::write(storage.snapshot_path(), b"not a sqry snapshot").expect("corrupt snapshot");
    (
        std::fs::read(storage.snapshot_path()).expect("snapshot bytes"),
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
    )
}

/// No hook: a missing index is `Ok(None)` with nothing written, and a
/// snapshot that fails to load is the loader's own `InvalidMagic` verdict
/// with the snapshot and manifest bytes untouched. The executor holds a
/// manager that can build over a source tree it can build (design D22),
/// so a restored direct build at either site of `get_or_load_graph`
/// answers `Ok(Some)` and rewrites both files, which the assertions name.
/// On `28f7337e1` the executor built with its own `plugin_manager` at both
/// sites and left a `.sqry/graph/manifest.json` behind.
#[test]
fn executor_without_a_build_hook_builds_nothing() {
    use crate::graph::unified::persistence::{GraphStorage, PersistenceError};

    assert_auto_index_gate_is_on();

    // Missing index over a buildable source tree. The manager is measured
    // against this same tree before the executor holds it.
    let empty = tempfile::TempDir::new().expect("tempdir");
    write_buildable_source(empty.path());
    let executor = QueryExecutor::with_plugin_manager(manager_that_can_build(empty.path()));
    assert!(
        !empty.path().join(".sqry").exists(),
        "fixture precondition: the in-memory build wrote nothing"
    );
    let outcome = executor.get_or_load_graph(empty.path());
    assert!(
        matches!(outcome, Ok(None)),
        "no hook and no index must be Ok(None); got {outcome:?}"
    );
    assert!(
        !empty.path().join(".sqry").exists(),
        "no hook must write nothing; found {:?}",
        std::fs::read_to_string(empty.path().join(".sqry/graph/manifest.json"))
    );

    // Snapshot that fails to load, beside a buildable source tree.
    let corrupt = tempfile::TempDir::new().expect("tempdir");
    write_buildable_source(corrupt.path());
    let (snapshot_before, manifest_before) = write_corrupt_index(corrupt.path());
    let outcome = executor.get_or_load_graph(corrupt.path());
    let err = match outcome {
        Ok(graph) => panic!(
            "no hook over a corrupt snapshot must be the load error; got Ok({:?})",
            graph.map(|g| g.node_count())
        ),
        Err(err) => err,
    };
    let verdict = err.downcast_ref::<PersistenceError>();
    assert!(
        matches!(verdict, Some(PersistenceError::InvalidMagic { .. })),
        "no hook over a corrupt snapshot must be the loader's InvalidMagic verdict; got {err:#}"
    );
    let storage = GraphStorage::new(corrupt.path());
    assert_eq!(
        std::fs::read(storage.snapshot_path()).expect("snapshot bytes"),
        snapshot_before,
        "no hook must leave the snapshot bytes as they are"
    );
    assert_eq!(
        std::fs::read(storage.manifest_path()).expect("manifest bytes"),
        manifest_before,
        "no hook must leave the manifest bytes as they are"
    );
    println!(
        "T38 corrupt leg: verdict={} snapshot_unchanged=true manifest_unchanged=true",
        verdict.map_or("none", |_| "InvalidMagic")
    );
}

/// With a hook: the hook is called exactly once for a missing graph, its
/// graph is returned and cached, so a second call is served from the cache
/// without calling it again. A compile error on `28f7337e1`
/// (`with_auto_build_hook` did not exist), recorded as the weaker evidence.
#[test]
fn executor_build_hook_is_called_once_per_missing_graph() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    assert_auto_index_gate_is_on();
    let calls = Arc::new(AtomicUsize::new(0));
    let built = Arc::new(CodeGraph::new());
    let hook: crate::graph::acquisition::AutoBuildHook = {
        let calls = Arc::clone(&calls);
        let built = Arc::clone(&built);
        Arc::new(move |_root: &Path| {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::clone(&built))
        })
    };
    let executor = QueryExecutor::with_plugin_manager(crate::plugin::PluginManager::new())
        .with_auto_build_hook(hook);

    let empty = tempfile::TempDir::new().expect("tempdir");
    let first = executor
        .get_or_load_graph(empty.path())
        .expect("the hook builds")
        .expect("the hook's graph is returned");
    assert!(
        Arc::ptr_eq(&first, &built),
        "the returned graph is the hook's"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one call for one missing graph"
    );

    let second = executor
        .get_or_load_graph(empty.path())
        .expect("the cache serves")
        .expect("the cached graph is returned");
    assert!(
        Arc::ptr_eq(&second, &built),
        "the second call is the cached graph"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the cache must serve the second call without calling the hook again"
    );
}
