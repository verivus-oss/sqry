use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_plugin_registry::create_plugin_manager;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use tempfile::TempDir;

#[test]
fn test_walk_flags_hidden_and_depth() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();

    // visible.rs
    fs::write(root.join("visible.rs"), "fn main() {}").unwrap();

    // .hidden/secret.rs
    let hidden_dir = root.join(".hidden");
    fs::create_dir(&hidden_dir).unwrap();
    fs::write(hidden_dir.join("secret.rs"), "fn secret() {}").unwrap();

    // d1/level1.rs
    let d1 = root.join("d1");
    fs::create_dir(&d1).unwrap();
    fs::write(d1.join("level1.rs"), "fn level1() {}").unwrap();

    // d1/d2/level2.rs
    let d2 = d1.join("d2");
    fs::create_dir(&d2).unwrap();
    fs::write(d2.join("level2.rs"), "fn level2() {}").unwrap();

    let plugins = create_plugin_manager();

    // 1. Default: visible.rs, level1.rs, level2.rs. (Hidden ignored)
    let config = BuildConfig::default();
    let graph = build_unified_graph(root, &plugins, &config).unwrap();
    assert_eq!(graph.files().len(), 3, "Expected visible, level1, level2");

    // 2. Hidden: +secret.rs
    let config = BuildConfig {
        include_hidden: true,
        ..Default::default()
    };
    let graph = build_unified_graph(root, &plugins, &config).unwrap();
    assert_eq!(graph.files().len(), 4, "Expected +secret.rs");

    // 3. Max depth 1 (root entries only, depth starts at 0 for the root path)
    let config = BuildConfig {
        max_depth: Some(1),
        ..Default::default()
    };
    let graph = build_unified_graph(root, &plugins, &config).unwrap();
    assert_eq!(
        graph.files().len(),
        1,
        "Expected only visible.rs at depth 1"
    );
}

#[cfg(unix)]
#[test]
fn test_walk_flags_symlinks() {
    let temp = TempDir::new().unwrap();
    let base = temp.path();

    let project = base.join("project");
    fs::create_dir(&project).unwrap();

    let external = base.join("external");
    fs::create_dir(&external).unwrap();

    fs::write(project.join("main.rs"), "fn main() {}").unwrap();
    fs::write(external.join("lib.rs"), "fn lib() {}").unwrap();

    // project/link -> ../external
    symlink(&external, project.join("link")).unwrap();

    let plugins = create_plugin_manager();

    // 1. Default (no follow)
    let config = BuildConfig::default();
    let graph = build_unified_graph(&project, &plugins, &config).unwrap();
    assert_eq!(graph.files().len(), 1, "Expected only main.rs");

    // 2. Follow links
    let config = BuildConfig {
        follow_links: true,
        ..Default::default()
    };
    let graph = build_unified_graph(&project, &plugins, &config).unwrap();
    assert_eq!(graph.files().len(), 2, "Expected main.rs and lib.rs");
}
