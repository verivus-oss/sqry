//! An incremental rebuild re-parses a changed file with the macro build
//! options the full build applied (surface parity W4).
//!
//! `incremental_rebuild` used to re-parse every closure file with
//! `MacroBuildOptions::default()`, whatever `BuildConfig::macro_options` it
//! was given: the changed file lost its cfg activation (`cfg_active`, the
//! `macroMetadata.cfgActive` a client reads) and every symbol the expand
//! cache materialised for it, which is the silently dropped cfg activation
//! W4 exists to close. Each test edits one file and rebuilds it
//! incrementally; the control from the other side rebuilds the same edit
//! with no options and shows what the options change.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sqry_core::graph::unified::build::incremental::{
    compute_reverse_dep_closure, incremental_rebuild,
};
use sqry_core::graph::unified::build::{
    BuildConfig, CancellationToken, MacroBuildOptions, build_unified_graph,
};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::find_nodes_by_name;
use sqry_core::plugin::{LanguagePlugin, PluginManager};
use sqry_lang_rust::RustPlugin;
use sqry_lang_rust::macro_boundaries::expand_cache::{
    EXPAND_CACHE_SCHEMA_VERSION, ExpandCache, ExpandCacheEntry, GeneratedSymbol,
    GeneratedSymbolKind, ScopeSegment, compute_crate_source_hash,
};
use tempfile::TempDir;

const CARGO_TOML: &str =
    "[package]\nname = \"incremental_crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

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

fn plugins() -> PluginManager {
    let mut pm = PluginManager::new();
    pm.register_builtin(Box::new(RustPlugin::default()));
    pm
}

struct Fixture {
    _crate_dir: TempDir,
    _cache_dir: TempDir,
    root: PathBuf,
    lib_rs: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let crate_dir = TempDir::new().expect("crate dir");
        let cache_dir = TempDir::new().expect("cache dir");
        let root = crate_dir.path().canonicalize().expect("canonical root");
        fs::write(root.join("Cargo.toml"), CARGO_TOML).expect("Cargo.toml");
        fs::create_dir_all(root.join("src")).expect("src");
        let lib_rs = root.join("src").join("lib.rs");
        fs::write(&lib_rs, LIB_RS).expect("lib.rs");
        let fixture = Self {
            cache: cache_dir.path().canonicalize().expect("canonical cache"),
            root,
            lib_rs,
            _crate_dir: crate_dir,
            _cache_dir: cache_dir,
        };
        fixture.write_fresh_cache();
        fixture
    }

    /// Write the cache entry for the crate as it is now, so the reader finds
    /// it fresh (its source hash matches).
    fn write_fresh_cache(&self) {
        let entry = ExpandCacheEntry {
            schema_version: EXPAND_CACHE_SCHEMA_VERSION,
            crate_name: "incremental_crate".to_string(),
            rust_version: "test".to_string(),
            generated_at: "0Z".to_string(),
            source_hash: compute_crate_source_hash(&self.root).expect("source hash"),
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
            .write("incremental_crate", &entry)
            .expect("write cache");
    }

    fn config(&self) -> BuildConfig {
        BuildConfig {
            macro_options: MacroBuildOptions {
                cfg_flags: vec!["test".to_string()],
                expand_cache_dir: Some(self.cache.clone()),
            },
            ..BuildConfig::default()
        }
    }

    /// Edit `lib.rs`, refresh the cache for the edited source, and rebuild
    /// the one changed file incrementally over `graph` with `config`.
    fn edit_and_rebuild(
        &self,
        graph: &CodeGraph,
        config: &BuildConfig,
    ) -> sqry_core::graph::error::GraphResult<CodeGraph> {
        fs::write(&self.lib_rs, LIB_RS_EDITED).expect("edit lib.rs");
        self.write_fresh_cache();
        rebuild_changed(graph, &self.lib_rs, config)
    }
}

fn rebuild_changed(
    graph: &CodeGraph,
    changed: &Path,
    config: &BuildConfig,
) -> sqry_core::graph::error::GraphResult<CodeGraph> {
    let file_id = graph.files().get(changed).expect("the file has a FileId");
    let closure: HashSet<_> = compute_reverse_dep_closure(&[file_id], graph);
    incremental_rebuild(
        graph,
        &[changed.to_path_buf()],
        &closure,
        &plugins(),
        config,
        &CancellationToken::new(),
    )
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

/// The full build applies `--cfg test` and the expand cache; an incremental
/// rebuild of the edited `lib.rs` with the same options keeps both: the
/// gated item is still active and the cache symbol is still materialised,
/// beside the function the edit added.
#[test]
fn an_incremental_rebuild_keeps_the_cfg_activation_and_the_expand_cache() {
    let fixture = Fixture::new();
    let config = fixture.config();
    let graph = build_unified_graph(&fixture.root, &plugins(), &config).expect("full build");
    assert_eq!(
        cfg_states(&graph),
        vec![("test".to_string(), Some(true))],
        "precondition: the full build activates cfg(test)"
    );
    assert!(
        has_generated(&graph, "widgets::Widget::derived_method"),
        "precondition: the full build materialises the cache symbol"
    );

    let rebuilt = fixture
        .edit_and_rebuild(&graph, &config)
        .expect("incremental rebuild");
    assert!(
        has_node(&rebuilt, "added_by_the_edit"),
        "the edited file was re-parsed"
    );
    assert_eq!(
        cfg_states(&rebuilt),
        vec![("test".to_string(), Some(true))],
        "the re-parsed file keeps its cfg activation"
    );
    assert!(
        has_generated(&rebuilt, "widgets::Widget::derived_method"),
        "the re-parsed file keeps its expand cache symbol"
    );
}

/// The other side: the same edit rebuilt with no macro options leaves the
/// gated item's activation unknown and materialises nothing, so the options
/// are what the test above observes, not a property of the fixture.
#[test]
fn an_incremental_rebuild_without_options_applies_none() {
    let fixture = Fixture::new();
    let graph =
        build_unified_graph(&fixture.root, &plugins(), &fixture.config()).expect("full build");
    let rebuilt = fixture
        .edit_and_rebuild(&graph, &BuildConfig::default())
        .expect("incremental rebuild");
    assert!(has_node(&rebuilt, "added_by_the_edit"));
    assert_eq!(cfg_states(&rebuilt), vec![("test".to_string(), None)]);
    assert!(!has_generated(&rebuilt, "widgets::Widget::derived_method"));
}

/// An expand cache that is gone when the incremental rebuild runs refuses
/// it, naming the directory, as the full build refuses; the reader never
/// creates the directory, so the refusal leaves nothing behind.
#[test]
fn an_incremental_rebuild_refuses_an_expand_cache_that_is_gone() {
    let fixture = Fixture::new();
    let config = fixture.config();
    let graph = build_unified_graph(&fixture.root, &plugins(), &config).expect("full build");
    fs::write(&fixture.lib_rs, LIB_RS_EDITED).expect("edit lib.rs");
    fs::remove_dir_all(&fixture.cache).expect("remove the cache");
    let err = match rebuild_changed(&graph, &fixture.lib_rs, &config) {
        Ok(_) => panic!("a rebuild resolved with a cache that is gone must be refused"),
        Err(err) => err,
    };
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&fixture.cache.display().to_string())
            && rendered.contains("is gone (before the re-parse"),
        "{rendered}"
    );
    assert!(!fixture.cache.exists(), "the refusal did not recreate it");
}

/// The Rust plugin, whose graph builder removes `dir` the first time it
/// runs once `armed` is set: the expand cache vanishing during the
/// incremental re-parse, after the check before it passed. The full build
/// runs unarmed, so it parses with the cache in place.
struct CacheRemovingRustPlugin {
    rust: RustPlugin,
    builder: CacheRemovingBuilder,
}

struct CacheRemovingBuilder {
    rust: RustPlugin,
    dir: PathBuf,
    armed: Arc<AtomicBool>,
}

impl sqry_core::graph::GraphBuilder for CacheRemovingBuilder {
    fn build_graph(
        &self,
        tree: &tree_sitter::Tree,
        content: &[u8],
        file: &Path,
        staging: &mut sqry_core::graph::unified::StagingGraph,
    ) -> sqry_core::graph::GraphResult<()> {
        if self.armed.load(Ordering::SeqCst) {
            let _ = fs::remove_dir_all(&self.dir);
        }
        self.rust
            .graph_builder()
            .expect("the Rust plugin builds graphs")
            .build_graph(tree, content, file, staging)
    }

    fn language(&self) -> sqry_core::graph::Language {
        sqry_core::graph::Language::Rust
    }
}

impl LanguagePlugin for CacheRemovingRustPlugin {
    fn metadata(&self) -> sqry_core::plugin::LanguageMetadata {
        self.rust.metadata()
    }

    fn extensions(&self) -> &'static [&'static str] {
        self.rust.extensions()
    }

    fn language(&self) -> tree_sitter::Language {
        self.rust.language()
    }

    fn parse_ast(
        &self,
        content: &[u8],
    ) -> Result<tree_sitter::Tree, sqry_core::plugin::error::ParseError> {
        self.rust.parse_ast(content)
    }

    fn extract_scopes(
        &self,
        tree: &tree_sitter::Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<sqry_core::ast::Scope>, sqry_core::plugin::error::ScopeError> {
        self.rust.extract_scopes(tree, content, file_path)
    }

    fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
        Some(&self.builder)
    }
}

/// An expand cache removed during the incremental re-parse refuses the
/// rebuild after the re-parse, naming the directory, instead of returning
/// a graph whose re-parsed files lost the cached symbols; the check before
/// the re-parse passed, so only the check after it can refuse. The control:
/// the same plugin unarmed rebuilds the edit.
#[test]
fn an_incremental_rebuild_refuses_an_expand_cache_removed_during_the_re_parse() {
    let fixture = Fixture::new();
    let config = fixture.config();
    let armed = Arc::new(AtomicBool::new(false));
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(CacheRemovingRustPlugin {
        rust: RustPlugin::default(),
        builder: CacheRemovingBuilder {
            rust: RustPlugin::default(),
            dir: fixture.cache.clone(),
            armed: Arc::clone(&armed),
        },
    }));
    let graph = build_unified_graph(&fixture.root, &plugins, &config).expect("full build");
    fs::write(&fixture.lib_rs, LIB_RS_EDITED).expect("edit lib.rs");
    fixture.write_fresh_cache();
    let file_id = graph.files().get(&fixture.lib_rs).expect("a FileId");
    let closure: HashSet<_> = compute_reverse_dep_closure(&[file_id], &graph);
    let rebuild = || {
        incremental_rebuild(
            &graph,
            std::slice::from_ref(&fixture.lib_rs),
            &closure,
            &plugins,
            &config,
            &CancellationToken::new(),
        )
    };
    let rebuilt = rebuild().expect("unarmed, the rebuild succeeds");
    assert!(
        has_node(&rebuilt, "added_by_the_edit"),
        "control: re-parsed"
    );

    armed.store(true, Ordering::SeqCst);
    let err = match rebuild() {
        Ok(_) => panic!("a cache removed during the re-parse must refuse the rebuild"),
        Err(err) => err,
    };
    let rendered = format!("{err:#}");
    assert!(
        rendered.contains(&fixture.cache.display().to_string())
            && rendered.contains("is gone (after the re-parse"),
        "{rendered}"
    );
    assert!(!fixture.cache.exists(), "the refusal did not recreate it");
}
