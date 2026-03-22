//! Parallel plugin safety smoke tests.
//!
//! Verifies that every plugin with a `GraphBuilder` can safely parse and build
//! graphs for two files concurrently via `rayon::join`, catching any shared
//! mutable parser state that would cause data races or panics.

use std::path::Path;

use anyhow::{Context, Result};
use sqry_core::graph::unified::build::StagingGraph;
use sqry_core::plugin::LanguagePlugin;
use walkdir::WalkDir;

// ---------------------------------------------------------------------------
// Plugin inventory (uses only dev-dependencies already declared in Cargo.toml)
// ---------------------------------------------------------------------------

fn all_plugins() -> Vec<Box<dyn LanguagePlugin>> {
    vec![
        Box::new(sqry_lang_rust::RustPlugin::default()),
        Box::new(sqry_lang_cpp::CppPlugin::default()),
        Box::new(sqry_lang_javascript::JavaScriptPlugin::default()),
        Box::new(sqry_lang_python::PythonPlugin::default()),
        Box::new(sqry_lang_typescript::TypeScriptPlugin::default()),
        Box::new(sqry_lang_go::GoPlugin::default()),
        Box::new(sqry_lang_java::JavaPlugin::default()),
        Box::new(sqry_lang_c::CPlugin::default()),
        Box::new(sqry_lang_dart::DartPlugin::default()),
        Box::new(sqry_lang_swift::SwiftPlugin::default()),
        Box::new(sqry_lang_kotlin::KotlinPlugin::default()),
        Box::new(sqry_lang_ruby::RubyPlugin::default()),
        Box::new(sqry_lang_php::PhpPlugin::default()),
        Box::new(sqry_lang_scala::ScalaPlugin::default()),
        Box::new(sqry_lang_lua::LuaPlugin::default()),
        Box::new(sqry_lang_r::RPlugin::default()),
        Box::new(sqry_lang_groovy::GroovyPlugin::default()),
        Box::new(sqry_lang_svelte::SveltePlugin::default()),
    ]
}

// ---------------------------------------------------------------------------
// Fixture discovery
// ---------------------------------------------------------------------------

/// Workspace root (two levels above `sqry-core/tests/`).
fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
}

/// Search well-known fixture directories for files matching any of the given
/// extensions.  Returns at most `limit` paths.
fn find_fixture_files(extensions: &[&str], limit: usize) -> Vec<std::path::PathBuf> {
    let root = workspace_root();

    // Directories that contain fixture source files.
    let search_dirs: Vec<std::path::PathBuf> = {
        let mut dirs = vec![root.join("test-fixtures")];
        // Also look through each sqry-lang-*/tests/fixtures/ directory.
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with("sqry-lang-") {
                    let fixture_dir = entry.path().join("tests").join("fixtures");
                    if fixture_dir.is_dir() {
                        dirs.push(fixture_dir);
                    }
                }
            }
        }
        dirs
    };

    let mut found = Vec::new();
    for dir in &search_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(dir).into_iter().filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let matches = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    extensions
                        .iter()
                        .any(|registered| registered.eq_ignore_ascii_case(ext))
                });
            if matches {
                found.push(path.to_path_buf());
                if found.len() >= limit {
                    return found;
                }
            }
        }
    }
    found
}

// ---------------------------------------------------------------------------
// Helper: parse + build for a single file
// ---------------------------------------------------------------------------

fn parse_and_build(plugin: &dyn LanguagePlugin, file_path: &Path) -> Result<StagingGraph> {
    let content =
        std::fs::read(file_path).with_context(|| format!("reading {}", file_path.display()))?;

    let tree = plugin
        .parse_ast(&content)
        .map_err(|e| anyhow::anyhow!("parse_ast failed for {}: {e}", file_path.display()))?;

    let builder = plugin
        .graph_builder()
        .ok_or_else(|| anyhow::anyhow!("no graph builder for plugin"))?;

    let mut staging = StagingGraph::new();
    builder
        .build_graph(&tree, &content, file_path, &mut staging)
        .map_err(|e| anyhow::anyhow!("build_graph failed for {}: {e}", file_path.display()))?;

    Ok(staging)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
fn parallel_plugin_graph_build_safety() {
    let plugins = all_plugins();
    let mut tested_count: usize = 0;

    for plugin in &plugins {
        let meta = plugin.metadata();

        // Skip plugins without a graph builder.
        if plugin.graph_builder().is_none() {
            eprintln!("[skip] {} — no graph builder", meta.name);
            continue;
        }

        let extensions: Vec<&str> = plugin.extensions().to_vec();
        let files = find_fixture_files(&extensions, 2);

        if files.len() < 2 {
            eprintln!(
                "[skip] {} — found only {} fixture file(s) for extensions {:?}",
                meta.name,
                files.len(),
                extensions,
            );
            continue;
        }

        let file_a = &files[0];
        let file_b = &files[1];

        eprintln!(
            "[test] {} — {} + {}",
            meta.name,
            file_a.display(),
            file_b.display(),
        );

        // Run two graph builds in parallel via rayon::join.
        let (result_a, result_b) = rayon::join(
            || parse_and_build(plugin.as_ref(), file_a),
            || parse_and_build(plugin.as_ref(), file_b),
        );

        // Both must succeed.
        let _staging_a = result_a.unwrap_or_else(|e| {
            panic!(
                "parallel build failed for {} on {}: {e:#}",
                meta.name,
                file_a.display()
            )
        });
        let _staging_b = result_b.unwrap_or_else(|e| {
            panic!(
                "parallel build failed for {} on {}: {e:#}",
                meta.name,
                file_b.display()
            )
        });

        tested_count += 1;
    }

    eprintln!("[result] {tested_count} plugin(s) tested in parallel");
    assert!(
        tested_count >= 18,
        "expected all 18 plugins tested but only {tested_count} passed; \
         check fixture files for skipped plugins"
    );
}
