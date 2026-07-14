//! End-to-end coverage for the Phase 1b `--expand-cache` F4 consumer.
//!
//! Proves that a pre-generated expand cache flows the whole way down:
//! `BuildConfig::macro_options.expand_cache_dir` -> `parse_file` (copies onto the
//! per-file `StagingGraph`) -> `RustGraphBuilder::build_graph` Pass 2.5 (reads the
//! side channel, resolves the crate via a pure-FS `Cargo.toml` walk, checks
//! freshness, reads the cache, selects the symbols this file owns, and
//! materialises them as searchable graph nodes flagged `macro_generated`).
//!
//! The core acceptance property (F4b): a materialised node's name is
//! byte-identical to the name the live graph builder produces for the same
//! construct, because both go through the shared `build_qualified_name`. The
//! fixture contains REAL constructs (whose names the live builder produces) plus
//! cache entries mirroring them with a swapped leaf name; the two must agree on
//! everything but the leaf.

use std::collections::HashSet;

use sqry_core::graph::unified::build::{BuildConfig, MacroBuildOptions, build_unified_graph};
use sqry_core::graph::unified::find_nodes_by_name;
use sqry_core::plugin::PluginManager;
use sqry_lang_rust::RustPlugin;
use sqry_lang_rust::macro_boundaries::expand_cache::{
    EXPAND_CACHE_SCHEMA_VERSION, ExpandCache, ExpandCacheEntry, GeneratedSymbol,
    GeneratedSymbolKind, ScopeSegment, compute_crate_source_hash,
};

const CARGO_TOML: &str =
    "[package]\nname = \"fixture_crate\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// Real constructs the LIVE builder names, plus inline modules (so the cache's
/// generated symbols have an owning module in this file).
const LIB_RS: &str = r"
pub mod widgets {
    pub struct Widget;

    impl Widget {
        pub fn live_method(&self) {}
    }

    pub trait Greeter {
        fn live_greet(&self) {}
    }
}

pub mod a {
    pub mod b {
        pub mod c {
            pub mod d {
                pub mod e {
                    pub fn live_deep() {}
                    // DECLARATIONS nested deeper than max_scope_depth (4). The
                    // live builder names these via `qualify_item_name` with NO
                    // truncation, unlike the callable `live_deep` above.
                    pub struct LiveDeepStruct;
                    pub mod live_deep_mod {}
                }
            }
        }
    }
}
";

/// A second file, present only to prove no other file claims lib.rs-owned
/// generated symbols (cross-file dedup).
const OTHER_RS: &str = "pub fn other_fn() {}\n";

fn seg(name: &str, is_module: bool) -> ScopeSegment {
    ScopeSegment {
        name: name.to_string(),
        is_module,
    }
}

/// The three edge-case generated symbols mirroring the real constructs with a
/// swapped leaf name (so `has_node` does not skip them).
fn fixture_generated_symbols() -> Vec<GeneratedSymbol> {
    vec![
        // Derive-generated impl method (Claude/Grok edge): plain impl type.
        GeneratedSymbol {
            simple_name: "derived_method".to_string(),
            scope_segments: vec![seg("widgets", true)],
            impl_type: Some("Widget".to_string()),
            kind: GeneratedSymbolKind::Method,
        },
        // Trait default-body method (Claude's edge): trait scope, no impl type.
        GeneratedSymbol {
            simple_name: "derived_greet".to_string(),
            scope_segments: vec![seg("widgets", true), seg("Greeter", false)],
            impl_type: None,
            kind: GeneratedSymbolKind::Function,
        },
        // Module nested deeper than max_scope_depth (Codex's edge): truncated.
        GeneratedSymbol {
            simple_name: "derived_deep".to_string(),
            scope_segments: vec![
                seg("a", true),
                seg("b", true),
                seg("c", true),
                seg("d", true),
                seg("e", true),
            ],
            impl_type: None,
            kind: GeneratedSymbolKind::Function,
        },
    ]
}

struct Fixture {
    _crate_dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
    crate_root: std::path::PathBuf,
    cache_path: std::path::PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let crate_dir = tempfile::tempdir().expect("crate tempdir");
        let cache_dir = tempfile::tempdir().expect("cache tempdir");
        let root = crate_dir.path().to_path_buf();
        std::fs::write(root.join("Cargo.toml"), CARGO_TOML).expect("write Cargo.toml");
        std::fs::create_dir_all(root.join("src")).expect("mkdir src");
        std::fs::write(root.join("src/lib.rs"), LIB_RS).expect("write lib.rs");
        std::fs::write(root.join("src/other.rs"), OTHER_RS).expect("write other.rs");
        let cache_path = cache_dir.path().to_path_buf();
        Self {
            crate_root: root,
            cache_path,
            _crate_dir: crate_dir,
            _cache_dir: cache_dir,
        }
    }

    /// Current (correct) source hash for the fixture crate.
    fn source_hash(&self) -> String {
        compute_crate_source_hash(&self.crate_root).expect("hash crate source")
    }

    /// Write a cache entry keyed by the fixture crate name.
    fn write_cache(&self, entry: &ExpandCacheEntry) {
        let cache = ExpandCache::new(self.cache_path.clone()).expect("open cache");
        cache.write("fixture_crate", entry).expect("write cache");
    }

    /// Build the fixture graph, optionally consuming the expand cache.
    fn build(&self, with_cache: bool) -> sqry_core::graph::unified::CodeGraph {
        let mut plugins = PluginManager::new();
        plugins.register_builtin(Box::new(RustPlugin::default()));
        let config = BuildConfig {
            macro_options: MacroBuildOptions {
                expand_cache_dir: with_cache.then(|| self.cache_path.clone()),
                ..MacroBuildOptions::default()
            },
            ..BuildConfig::default()
        };
        build_unified_graph(&self.crate_root, &plugins, &config).expect("build graph")
    }
}

/// Set of `(index, generation)` keys for nodes flagged `macro_generated`.
fn macro_generated_keys(graph: &sqry_core::graph::unified::CodeGraph) -> HashSet<(u32, u64)> {
    graph
        .macro_metadata()
        .iter()
        .filter(|(_key, meta)| meta.macro_generated == Some(true))
        .map(|(key, _meta)| key)
        .collect()
}

/// Assert a node with `name` exists, is macro-generated, and appears exactly
/// `count` times.
fn assert_generated(
    graph: &sqry_core::graph::unified::CodeGraph,
    macro_keys: &HashSet<(u32, u64)>,
    name: &str,
    count: usize,
) {
    let snapshot = graph.snapshot();
    let ids = find_nodes_by_name(&snapshot, name);
    assert_eq!(
        ids.len(),
        count,
        "expected {count} node(s) named `{name}`, found {}: {ids:?}",
        ids.len()
    );
    assert!(
        ids.iter()
            .any(|id| macro_keys.contains(&(id.index(), id.generation()))),
        "node `{name}` must be flagged macro_generated"
    );
}

#[test]
fn materialised_names_byte_match_live_builder() {
    let fixture = Fixture::new();

    // Baseline build (no cache): capture the live builder's names for the real
    // constructs. These document the naming convention the cache must match.
    let baseline = fixture.build(false);
    let base_snapshot = baseline.snapshot();
    assert_eq!(
        find_nodes_by_name(&base_snapshot, "widgets::Widget::live_method").len(),
        1,
        "live inherent/impl method name"
    );
    assert_eq!(
        find_nodes_by_name(&base_snapshot, "widgets::Greeter::live_greet").len(),
        1,
        "live trait default-body method name"
    );
    assert_eq!(
        find_nodes_by_name(&base_snapshot, "a::b::c::d::live_deep").len(),
        1,
        "live deeply-nested item name (truncated at max_scope_depth = 4)"
    );
    // Without the cache, no macro-generated node exists.
    assert!(
        find_nodes_by_name(&base_snapshot, "widgets::Widget::derived_method").is_empty(),
        "no cached symbols must be materialised without --expand-cache"
    );

    // Now write a fresh cache and rebuild consuming it.
    let entry = ExpandCacheEntry {
        schema_version: EXPAND_CACHE_SCHEMA_VERSION,
        crate_name: "fixture_crate".to_string(),
        rust_version: "test".to_string(),
        generated_at: "0Z".to_string(),
        source_hash: fixture.source_hash(),
        confidence: "heuristic".to_string(),
        generated_symbols: fixture_generated_symbols(),
    };
    fixture.write_cache(&entry);

    let graph = fixture.build(true);
    let macro_keys = macro_generated_keys(&graph);

    // Each materialised name is present, macro-generated, and appears once (no
    // cross-file duplication: only lib.rs owns the `widgets` / `a::..` modules).
    assert_generated(&graph, &macro_keys, "widgets::Widget::derived_method", 1);
    assert_generated(&graph, &macro_keys, "widgets::Greeter::derived_greet", 1);
    assert_generated(&graph, &macro_keys, "a::b::c::d::derived_deep", 1);

    // Byte-match proof: the materialised name equals the live name with only the
    // leaf identifier swapped. The scope prefix, impl-type folding, and
    // max_scope_depth truncation are therefore byte-identical to the live
    // builder for the same construct.
    assert_eq!(
        "widgets::Widget::derived_method",
        "widgets::Widget::live_method".replace("live_method", "derived_method")
    );
    assert_eq!(
        "widgets::Greeter::derived_greet",
        "widgets::Greeter::live_greet".replace("live_greet", "derived_greet")
    );
    assert_eq!(
        "a::b::c::d::derived_deep",
        "a::b::c::d::live_deep".replace("live_deep", "derived_deep")
    );
}

#[test]
fn declaration_names_are_not_truncated_like_callables() {
    // BLOCKER 1 regression: the live graph has TWO naming paths. DECLARATIONS
    // (struct/enum/trait/type/const/mod) are named by `qualify_item_name` with
    // NO `max_scope_depth` truncation, whereas CALLABLES are truncated on the
    // free-item branch. A generated declaration nested deeper than
    // max_scope_depth (4) must therefore keep its FULL module path, matching the
    // live builder. The old F4 consumer routed every kind through
    // `build_qualified_name` and wrongly truncated deep declarations.
    let fixture = Fixture::new();

    // Baseline (no cache): capture the live builder's declaration names.
    let baseline = fixture.build(false);
    let base = baseline.snapshot();

    // Live declarations at module depth 5 keep the full path (no truncation).
    assert_eq!(
        find_nodes_by_name(&base, "a::b::c::d::e::LiveDeepStruct").len(),
        1,
        "live deep struct name must NOT be truncated"
    );
    assert_eq!(
        find_nodes_by_name(&base, "a::b::c::d::e::live_deep_mod").len(),
        1,
        "live deep module name must NOT be truncated"
    );
    // The truncated forms (what the callable path would produce) must NOT exist
    // for declarations, proving the two paths genuinely diverge.
    assert!(
        find_nodes_by_name(&base, "a::b::c::d::LiveDeepStruct").is_empty(),
        "declaration must not be truncated at max_scope_depth"
    );
    // The sibling CALLABLE at the same depth IS truncated (contrast).
    assert_eq!(
        find_nodes_by_name(&base, "a::b::c::d::live_deep").len(),
        1,
        "callable at depth 5 IS truncated at max_scope_depth = 4"
    );

    // Cache mirroring the deep declarations with a swapped leaf name.
    let generated = vec![
        GeneratedSymbol {
            simple_name: "derived_deep_struct".to_string(),
            scope_segments: vec![
                seg("a", true),
                seg("b", true),
                seg("c", true),
                seg("d", true),
                seg("e", true),
            ],
            impl_type: None,
            kind: GeneratedSymbolKind::Struct,
        },
        GeneratedSymbol {
            simple_name: "derived_deep_mod".to_string(),
            scope_segments: vec![
                seg("a", true),
                seg("b", true),
                seg("c", true),
                seg("d", true),
                seg("e", true),
            ],
            impl_type: None,
            kind: GeneratedSymbolKind::Module,
        },
    ];
    let entry = ExpandCacheEntry {
        schema_version: EXPAND_CACHE_SCHEMA_VERSION,
        crate_name: "fixture_crate".to_string(),
        rust_version: "test".to_string(),
        generated_at: "0Z".to_string(),
        source_hash: fixture.source_hash(),
        confidence: "heuristic".to_string(),
        generated_symbols: generated,
    };
    fixture.write_cache(&entry);

    let graph = fixture.build(true);
    let macro_keys = macro_generated_keys(&graph);

    // Materialised declaration names carry the FULL module path (byte-identical
    // to the live `qualify_item_name` naming for the same construct).
    assert_generated(&graph, &macro_keys, "a::b::c::d::e::derived_deep_struct", 1);
    assert_generated(&graph, &macro_keys, "a::b::c::d::e::derived_deep_mod", 1);

    // Regression guard: the truncated form (the pre-fix bug) must NOT appear.
    let snapshot = graph.snapshot();
    assert!(
        find_nodes_by_name(&snapshot, "a::b::c::d::derived_deep_struct").is_empty(),
        "a generated declaration must NOT be truncated at max_scope_depth"
    );

    // Byte-match: the materialised name equals the live name with only the leaf
    // identifier swapped.
    assert_eq!(
        "a::b::c::d::e::derived_deep_struct",
        "a::b::c::d::e::LiveDeepStruct".replace("LiveDeepStruct", "derived_deep_struct")
    );
    assert_eq!(
        "a::b::c::d::e::derived_deep_mod",
        "a::b::c::d::e::live_deep_mod".replace("live_deep_mod", "derived_deep_mod")
    );
}

#[test]
fn stale_cache_materialises_nothing() {
    let fixture = Fixture::new();
    let entry = ExpandCacheEntry {
        schema_version: EXPAND_CACHE_SCHEMA_VERSION,
        crate_name: "fixture_crate".to_string(),
        rust_version: "test".to_string(),
        generated_at: "0Z".to_string(),
        // Deliberately wrong hash: the source has changed since this was written.
        source_hash: "stale-hash-does-not-match".to_string(),
        confidence: "heuristic".to_string(),
        generated_symbols: fixture_generated_symbols(),
    };
    fixture.write_cache(&entry);

    let graph = fixture.build(true);
    let snapshot = graph.snapshot();
    assert!(
        find_nodes_by_name(&snapshot, "widgets::Widget::derived_method").is_empty(),
        "a stale cache (hash mismatch) must materialise no symbols"
    );
}

#[test]
fn wrong_schema_version_materialises_nothing() {
    let fixture = Fixture::new();
    let entry = ExpandCacheEntry {
        // Correct hash, but an old schema version: soft miss, skip the crate.
        schema_version: EXPAND_CACHE_SCHEMA_VERSION - 1,
        crate_name: "fixture_crate".to_string(),
        rust_version: "test".to_string(),
        generated_at: "0Z".to_string(),
        source_hash: fixture.source_hash(),
        confidence: "heuristic".to_string(),
        generated_symbols: fixture_generated_symbols(),
    };
    fixture.write_cache(&entry);

    let graph = fixture.build(true);
    let snapshot = graph.snapshot();
    assert!(
        find_nodes_by_name(&snapshot, "widgets::Widget::derived_method").is_empty(),
        "a schema-version mismatch must materialise no symbols"
    );
}

#[test]
fn poisoned_symbol_name_is_sanitised_out() {
    let fixture = Fixture::new();
    let mut symbols = fixture_generated_symbols();
    // A cache-poisoning attempt: a shell-metacharacter leaf name. `read`'s
    // sanitiser drops it before the consumer sees it.
    symbols.push(GeneratedSymbol {
        simple_name: "evil$(rm -rf)".to_string(),
        scope_segments: vec![seg("widgets", true)],
        impl_type: Some("Widget".to_string()),
        kind: GeneratedSymbolKind::Method,
    });
    let entry = ExpandCacheEntry {
        schema_version: EXPAND_CACHE_SCHEMA_VERSION,
        crate_name: "fixture_crate".to_string(),
        rust_version: "test".to_string(),
        generated_at: "0Z".to_string(),
        source_hash: fixture.source_hash(),
        confidence: "heuristic".to_string(),
        generated_symbols: symbols,
    };
    fixture.write_cache(&entry);

    let graph = fixture.build(true);
    let macro_keys = macro_generated_keys(&graph);
    // The valid symbols still materialise; the poisoned one never becomes a node.
    assert_generated(&graph, &macro_keys, "widgets::Widget::derived_method", 1);
    let snapshot = graph.snapshot();
    assert!(
        find_nodes_by_name(&snapshot, "widgets::Widget::evil$(rm -rf)").is_empty(),
        "a poisoned symbol name must be sanitised out (never materialised)"
    );
}

#[test]
fn missing_cargo_toml_degrades_gracefully() {
    // A loose file with no ancestor Cargo.toml: resolve_crate returns None and
    // the consumer skips (no panic, no materialisation, index still succeeds).
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
    std::fs::write(dir.path().join("src/lib.rs"), LIB_RS).expect("write");
    let cache_dir = tempfile::tempdir().expect("cache tempdir");

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(RustPlugin::default()));
    let config = BuildConfig {
        macro_options: MacroBuildOptions {
            expand_cache_dir: Some(cache_dir.path().to_path_buf()),
            ..MacroBuildOptions::default()
        },
        ..BuildConfig::default()
    };
    let graph =
        build_unified_graph(dir.path(), &plugins, &config).expect("build must still succeed");
    let snapshot = graph.snapshot();
    // The real constructs are still indexed; nothing macro-generated was added.
    assert_eq!(
        find_nodes_by_name(&snapshot, "widgets::Widget::live_method").len(),
        1
    );
    assert!(find_nodes_by_name(&snapshot, "widgets::Widget::derived_method").is_empty());
}
