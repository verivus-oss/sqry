//! Shared integration-test infrastructure for the Go T1 method-set /
//! implements/promotion AC tests.
//!
//! Implements `build_workspace(fixture_dir: &Path) -> CodeGraph` per
//! `docs/development/go-implements-and-promotion/02_DESIGN.md` §10.2 lines
//! 2293-2319. Each AC test in `implements_implicit.rs`, `promotion.rs`,
//! `signature_implements.rs`, and `determinism.rs` calls
//! `common::build_workspace(...)` to obtain a finalized `CodeGraph`
//! produced by the full `build_unified_graph` pipeline (Phases 1-4 +
//! Pass 5 + the Go method-set pass between Phase 4e and Pass 5).
//!
//! Why a hand-rolled `PluginManager`: `sqry-plugin-registry` depends on
//! `sqry-lang-go` (and 26 other plugins), so this crate's tests cannot
//! re-use `sqry_plugin_registry::create_plugin_manager` without a cycle.
//! Every fixture in this directory is Go-only, so registering just
//! `GoPlugin` is sufficient. See `05_TEST_PLAN.md` §5.

use sqry_core::graph::unified::CodeGraph;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::plugin::PluginManager;
use sqry_lang_go::GoPlugin;
use std::path::Path;

/// Build a Go fixture directory into a finalized `CodeGraph` via the
/// canonical `build_unified_graph` pipeline.
///
/// Mirrors `sqry-cli/tests/unified_graph_entrypoint.rs`'s setup, scoped
/// down to just the Go plugin.
pub fn build_workspace(fixture_dir: &Path) -> CodeGraph {
    assert!(
        fixture_dir.exists(),
        "fixture dir does not exist: {}",
        fixture_dir.display(),
    );
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(fixture_dir, &plugins, &config).unwrap_or_else(|err| {
        panic!(
            "build_unified_graph({}) failed: {err}",
            fixture_dir.display()
        )
    })
}
