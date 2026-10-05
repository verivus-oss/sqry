//! A watcher-driven rebuild of a changed file keeps the macro build options
//! the index records (integration round 7, coordinator item 7).
//!
//! The core's `incremental_rebuild` used to re-parse a changed file with no
//! macro options, dropping its cfg activation (`macroMetadata.cfgActive`)
//! and every symbol the expand cache materialised for it
//! (`sqry-core/tests/incremental_macro_options.rs`). The daemon's
//! watcher-driven rebuild does not call it: every iteration, the
//! incremental-triggered ones included, builds the whole graph with the
//! inputs resolved from the manifest, which carry the recorded options
//! (`RebuildDispatcher::resolve_rebuild_inputs`, `execute_rebuild_blocking`).
//! This pins that end to end: an index recorded with `--cfg test` and an
//! expand cache, a resident workspace, an edit the watcher picks up, and
//! the generation it publishes. The control from the other side indexes
//! with no options and shows what the options change.

#![cfg(all(unix, feature = "test-hooks"))]

mod support;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use sqry_core::graph::unified::build::{BuildConfig, MacroBuildOptions};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::find_nodes_by_name;
use sqry_core::project::{ProjectRootMode, canonicalize_path};
use sqry_daemon::{DaemonConfig, RebuildMode, TestCapture, WorkspaceKey};
use sqry_lang_rust::macro_boundaries::expand_cache::{
    EXPAND_CACHE_SCHEMA_VERSION, ExpandCache, ExpandCacheEntry, GeneratedSymbol,
    GeneratedSymbolKind, ScopeSegment, compute_crate_source_hash,
};
use support::ipc::expect_success;
use support::rebuild_fixtures::{
    ipc_client, manifest_json, mcp_call, mcp_payload, mcp_session, real_server, wait_until_blocking,
};

const CRATE: &str = "watched_crate";

const LIB_RS: &str = "\
#[cfg(test)]
pub fn gated_by_test() -> u32 { 1 }

pub mod widgets {
    pub struct Widget;

    impl Widget {
        pub fn live_method(&self) {}
    }
}
";

/// The same file after an edit: one function added, the rest unchanged.
const LIB_RS_EDITED: &str = "\
#[cfg(test)]
pub fn gated_by_test() -> u32 { 1 }

pub fn added_by_the_edit() {}

pub mod widgets {
    pub struct Widget;

    impl Widget {
        pub fn live_method(&self) {}
    }
}
";

struct Fixture {
    _crate_dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
    root: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    /// A git repository holding one crate, and an expand cache outside it
    /// whose entry is fresh for the crate's current source.
    fn new() -> Self {
        let crate_dir = tempfile::tempdir().expect("crate dir");
        let cache_dir = tempfile::tempdir().expect("cache dir");
        let root = canonicalize_path(crate_dir.path()).expect("canonical root");
        support::init_git_repo(&root);
        std::fs::write(
            root.join("Cargo.toml"),
            format!("[package]\nname = \"{CRATE}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .expect("Cargo.toml");
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src").join("lib.rs"), LIB_RS).expect("lib.rs");
        let fixture = Self {
            cache: canonicalize_path(cache_dir.path()).expect("canonical cache"),
            root,
            _crate_dir: crate_dir,
            _cache_dir: cache_dir,
        };
        let hash = compute_crate_source_hash(&fixture.root).expect("source hash");
        fixture.write_cache(hash);
        fixture
    }

    fn write_cache(&self, source_hash: String) {
        let entry = ExpandCacheEntry {
            schema_version: EXPAND_CACHE_SCHEMA_VERSION,
            crate_name: CRATE.to_string(),
            rust_version: "test".to_string(),
            generated_at: "0Z".to_string(),
            source_hash,
            confidence: "heuristic".to_string(),
            generated_symbols: vec![GeneratedSymbol {
                simple_name: "derived_method".to_string(),
                scope_segments: vec![ScopeSegment {
                    name: "widgets".to_string(),
                    is_module: true,
                }],
                impl_type: Some("Widget".to_string()),
                kind: GeneratedSymbolKind::Method,
            }],
        };
        ExpandCache::new(self.cache.clone())
            .expect("open cache")
            .write(CRATE, &entry)
            .expect("write cache");
    }

    /// Refresh the cache entry for the edited source before the edit lands,
    /// so the rebuild the edit triggers finds it fresh whenever it runs:
    /// the source hash covers the `.rs` files' bytes in path order, so it
    /// is computed over a copy of the crate's `.rs` files with the edit.
    fn refresh_cache_for_the_edit(&self) {
        let mirror = tempfile::tempdir().expect("mirror");
        std::fs::write(
            mirror.path().join("seed.rs"),
            std::fs::read(self.root.join("seed.rs")).expect("seed.rs"),
        )
        .expect("mirror seed.rs");
        std::fs::create_dir_all(mirror.path().join("src")).expect("mirror src");
        std::fs::write(mirror.path().join("src").join("lib.rs"), LIB_RS_EDITED)
            .expect("mirror lib.rs");
        let hash = compute_crate_source_hash(mirror.path()).expect("edited hash");
        self.write_cache(hash);
    }

    fn index(&self, macro_options: MacroBuildOptions) {
        let plugins = sqry_plugin_registry::create_plugin_manager();
        let ids: Vec<String> = plugins
            .plugins()
            .iter()
            .map(|plugin| plugin.metadata().id.to_string())
            .collect();
        sqry_core::graph::unified::build::build_and_persist_graph_with_progress(
            &self.root,
            &plugins,
            &BuildConfig {
                macro_options,
                ..BuildConfig::default()
            },
            "test:watcher_macro_options",
            Some(
                sqry_core::graph::unified::persistence::PluginSelectionManifest {
                    active_plugin_ids: ids,
                    high_cost_mode: Some("fast_path_default".to_string()),
                },
            ),
            sqry_core::progress::no_op_reporter(),
        )
        .expect("index persists");
    }
}

/// `(cfg_condition, cfg_active)` for every node carrying a cfg condition.
fn cfg_states(graph: &CodeGraph) -> Vec<(String, Option<bool>)> {
    graph
        .macro_metadata()
        .iter()
        .filter_map(|(_id, meta)| {
            meta.cfg_condition
                .clone()
                .map(|condition| (condition, meta.cfg_active))
        })
        .collect()
}

/// `true` when a node named `name` exists and is flagged `macro_generated`.
fn has_generated(graph: &CodeGraph, name: &str) -> bool {
    let snapshot = graph.snapshot();
    let generated: HashSet<(u32, u64)> = graph
        .macro_metadata()
        .iter()
        .filter(|(_key, meta)| meta.macro_generated == Some(true))
        .map(|(key, _meta)| key)
        .collect();
    find_nodes_by_name(&snapshot, name)
        .iter()
        .any(|id| generated.contains(&(id.index(), id.generation())))
}

fn has_node(graph: &CodeGraph, name: &str) -> bool {
    !find_nodes_by_name(&graph.snapshot(), name).is_empty()
}

/// The `macroMetadata.cfgActive` a client reads for `gated_by_test`.
async fn cfg_active_seen_by_a_client(
    peer: &rmcp::Peer<rmcp::RoleClient>,
    root: &Path,
) -> Option<Value> {
    let payload = mcp_payload(
        "search",
        &mcp_call(
            peer,
            "semantic_search",
            json!({ "path": root.to_string_lossy(), "query": "gated_by_test" }),
        )
        .await,
    );
    payload["data"]["results"]
        .as_array()?
        .iter()
        .find(|result| result["name"] == "gated_by_test")
        .map(|result| result["macroMetadata"]["cfgActive"].clone())
}

/// What one edit under the watcher shows: the graph before and after, and
/// the `cfgActive` a client reads for the gated item before and after.
struct Observed {
    before: Arc<CodeGraph>,
    after: Arc<CodeGraph>,
    client_before: Option<Value>,
    client_after: Option<Value>,
    /// The mode of each iteration the watcher ran.
    modes: Vec<RebuildMode>,
}

/// Load the indexed workspace, edit `lib.rs`, and wait for the watcher's
/// rebuild to publish the edited file.
async fn edit_under_the_watcher(fixture: &Fixture) -> Observed {
    // Every closure is small enough for an incremental iteration, so the
    // edit takes the incremental-triggered path (the one the core defect
    // was in), which the capture confirms.
    let (server, _builder) = real_server(DaemonConfig {
        debounce_ms: 200,
        closure_limit_percent: 100,
        ..DaemonConfig::default()
    })
    .await;
    let capture = Arc::new(TestCapture::new());
    server
        .dispatcher
        .install_test_capture(Arc::clone(&capture))
        .expect("capture installs");
    let key = WorkspaceKey::new(fixture.root.clone(), ProjectRootMode::GitRoot, 0);
    let mut client = ipc_client(&server).await;
    expect_success(
        &client
            .request(
                "daemon/load",
                json!({ "index_root": fixture.root.to_string_lossy() }),
            )
            .await,
    );
    let ws = server.manager.lookup(&key).expect("resident");
    let before = ws.graph();
    let running = mcp_session(&server).await;
    let client_before = cfg_active_seen_by_a_client(running.peer(), &fixture.root).await;
    fixture.refresh_cache_for_the_edit();
    std::fs::write(fixture.root.join("src").join("lib.rs"), LIB_RS_EDITED).expect("edit lib.rs");
    let published = {
        let ws = Arc::clone(&ws);
        tokio::task::spawn_blocking(move || {
            wait_until_blocking(Duration::from_secs(60), || {
                has_node(&ws.graph(), "added_by_the_edit")
            })
        })
        .await
        .expect("join")
    };
    assert!(published, "the watcher rebuilt the edited file");
    let after = ws.graph();
    let client_after = cfg_active_seen_by_a_client(running.peer(), &fixture.root).await;
    println!("client cfgActive: before={client_before:?} after={client_after:?}");
    drop(running);
    drop(client);
    server.stop().await;
    let modes = capture
        .iterations
        .lock()
        .iter()
        .map(|iteration| iteration.mode)
        .collect();
    Observed {
        before,
        after,
        client_before,
        client_after,
        modes,
    }
}

/// The index records `--cfg test` and the expand cache; the watcher's
/// rebuild of the edited `lib.rs` keeps both (the gated item active, the
/// cache symbol materialised, both still recorded), beside the function
/// the edit added.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watcher_rebuild_keeps_the_cfg_activation_and_the_expand_cache() {
    let fixture = Fixture::new();
    fixture.index(MacroBuildOptions {
        cfg_flags: vec!["test".to_string()],
        expand_cache_dir: Some(fixture.cache.clone()),
    });
    let Observed {
        before,
        after,
        client_before,
        client_after,
        modes,
    } = edit_under_the_watcher(&fixture).await;
    assert_eq!(
        modes,
        vec![RebuildMode::Incremental],
        "the edit ran one incremental-triggered iteration"
    );
    println!(
        "recorded options: before cfg={:?} generated={}; after cfg={:?} generated={}",
        cfg_states(&before),
        has_generated(&before, "widgets::Widget::derived_method"),
        cfg_states(&after),
        has_generated(&after, "widgets::Widget::derived_method"),
    );
    assert_eq!(
        cfg_states(&before),
        vec![("test".to_string(), Some(true))],
        "precondition: the load activates cfg(test)"
    );
    assert!(
        has_generated(&before, "widgets::Widget::derived_method"),
        "precondition: the load materialises the cache symbol"
    );
    assert_eq!(
        cfg_states(&after),
        vec![("test".to_string(), Some(true))],
        "the rebuilt file keeps its cfg activation"
    );
    assert!(
        has_generated(&after, "widgets::Widget::derived_method"),
        "the rebuilt file keeps its expand cache symbol"
    );
    assert_eq!(
        client_before,
        Some(json!(true)),
        "precondition: a client reads it active"
    );
    assert_eq!(
        client_after,
        Some(json!(true)),
        "a client still reads it active"
    );
    let record = manifest_json(&fixture.root).expect("manifest")["macro_options"].clone();
    assert_eq!(record["cfg_flags"], json!(["test"]), "{record}");
    assert_eq!(
        record["expand_cache_dir"],
        json!(fixture.cache.to_string_lossy()),
        "{record}"
    );
}

/// The other side: indexed with no options, the same edit rebuilt by the
/// watcher leaves the gated item's activation unknown and materialises
/// nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_watcher_rebuild_without_recorded_options_applies_none() {
    let fixture = Fixture::new();
    fixture.index(MacroBuildOptions::default());
    let observed = edit_under_the_watcher(&fixture).await;
    assert_eq!(observed.modes, vec![RebuildMode::Incremental]);
    assert_eq!(
        cfg_states(&observed.after),
        vec![("test".to_string(), None)]
    );
    assert!(!has_generated(
        &observed.after,
        "widgets::Widget::derived_method"
    ));
    assert_ne!(observed.client_after, Some(json!(true)));
}
