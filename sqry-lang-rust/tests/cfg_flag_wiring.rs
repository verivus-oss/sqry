//! End-to-end coverage for the Phase 1a `--cfg` config channel.
//!
//! Proves that cfg predicate strings flow the whole way down:
//! `BuildConfig::macro_options` -> `parse_file` (copies onto the per-file
//! `StagingGraph`) -> `RustGraphBuilder::build_graph` Pass 2.5 (reads the side
//! channel, builds `MacroBoundaryConfig`, splits `feature=<name>` tokens) ->
//! `cfg_analysis` -> the built graph's macro metadata. A cfg-gated item's
//! `cfg_active` flips from `None` (no `--cfg`) to `Some(true)` / `Some(false)`
//! once the matching flag is supplied.

use sqry_core::graph::unified::build::{BuildConfig, MacroBuildOptions, build_unified_graph};
use sqry_core::plugin::PluginManager;
use sqry_lang_rust::RustPlugin;

/// Two feature-gated free functions. `gated_on` is active under
/// `--cfg feature=x`; `gated_off` is not (its feature is never supplied).
const FIXTURE: &str = r#"
#[cfg(feature = "x")]
pub fn gated_on() {}

#[cfg(feature = "other")]
pub fn gated_off() {}
"#;

/// Build the fixture crate with the given `--cfg` flags and return the
/// `(cfg_condition, cfg_active)` pairs recorded in the graph's macro metadata.
fn cfg_conditions(cfg_flags: Vec<String>) -> Vec<(String, Option<bool>)> {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("lib.rs"), FIXTURE).expect("write fixture");

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(RustPlugin::default()));

    let config = BuildConfig {
        macro_options: MacroBuildOptions {
            cfg_flags,
            ..MacroBuildOptions::default()
        },
        ..BuildConfig::default()
    };

    let graph = build_unified_graph(tmp.path(), &plugins, &config).expect("build graph");

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

#[test]
fn cfg_flag_absent_leaves_cfg_active_unknown() {
    let conditions = cfg_conditions(Vec::new());

    // Both gated items are recorded with a cfg condition, but with no `--cfg`
    // supplied the activation stays unknown (`None`) exactly as before Phase 1a.
    assert_eq!(
        conditions.len(),
        2,
        "expected both cfg-gated items to be recorded: {conditions:?}"
    );
    assert!(
        conditions.iter().all(|(_, active)| active.is_none()),
        "without --cfg, cfg_active must stay None: {conditions:?}"
    );
}

#[test]
fn cfg_feature_flag_flips_activation() {
    let conditions = cfg_conditions(vec!["feature=x".to_string()]);

    let active_x = conditions
        .iter()
        .find(|(cond, _)| cond.contains("\"x\""))
        .map(|(_, active)| *active);
    let active_other = conditions
        .iter()
        .find(|(cond, _)| cond.contains("\"other\""))
        .map(|(_, active)| *active);

    // `--cfg feature=x` makes `cfg(feature = "x")` active and
    // `cfg(feature = "other")` inactive: the flag was split into the features
    // axis and evaluated per predicate.
    assert_eq!(
        active_x,
        Some(Some(true)),
        "cfg(feature=\"x\") must be active under --cfg feature=x: {conditions:?}"
    );
    assert_eq!(
        active_other,
        Some(Some(false)),
        "cfg(feature=\"other\") must be inactive under --cfg feature=x: {conditions:?}"
    );
}
