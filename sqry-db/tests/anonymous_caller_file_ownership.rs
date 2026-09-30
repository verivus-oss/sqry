//! Issue #850: a closure caller keeps the file it was parsed from.
//!
//! The Rust plugin names a closure from its own line, and the enclosing
//! module is very often just `tests`, so two crates routinely mint the same
//! qualified name (`tests::<anon:closure@N>`) for unrelated closures. If the
//! build treats that string as a workspace-wide identity, the two closures
//! merge and one crate's call edge is retargeted onto the other crate's node:
//! `direct-callers` then names a file that contains no such call, or drops the
//! caller entirely when the survivor was already in the list.
//!
//! These tests drive the real build (`build_unified_graph` + `RustPlugin`) and
//! the real callers primitive the CLI `graph direct-callers` and the MCP
//! `direct_callers` tool both route through
//! (`sqry_db::queries::dispatch::mcp_callers_query`).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::plugin::PluginManager;
use sqry_db::queries::RelationKey;
use sqry_db::queries::dispatch::mcp_callers_query;
use sqry_db::{QueryDb, QueryDbConfig};
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

/// Relative path of the first crate's only source file.
const FILE_A: &str = "crate_a/src/management.rs";
/// Relative path of the second crate's only source file.
const FILE_B: &str = "crate_b/src/acquirer.rs";

/// Source for the first crate. The closure that calls `shared_helper` sits on
/// the same line as the one in [`source_b`], which is the whole point of the
/// fixture, so keep the two bodies line-aligned when editing either.
fn source_a() -> &'static str {
    "//! Crate A.\n\
     \n\
     pub fn shared_helper(value: u32) -> u32 {\n\
     \x20   value + 1\n\
     }\n\
     \n\
     #[cfg(test)]\n\
     mod tests {\n\
     \x20   use super::shared_helper;\n\
     \n\
     \x20   #[test]\n\
     \x20   fn alpha_case() {\n\
     \x20       let apply = |n: u32| shared_helper(n);\n\
     \x20       assert_eq!(apply(1), 2);\n\
     \x20   }\n\
     }\n"
}

/// Source for the second crate, line-aligned with [`source_a`].
fn source_b() -> &'static str {
    "//! Crate B.\n\
     \n\
     pub fn other_helper(value: u32) -> u32 {\n\
     \x20   value + 2\n\
     }\n\
     \n\
     #[cfg(test)]\n\
     mod tests {\n\
     \x20   use super::shared_helper;\n\
     \n\
     \x20   #[test]\n\
     \x20   fn beta_case() {\n\
     \x20       let apply = |n: u32| shared_helper(n);\n\
     \x20       assert_eq!(apply(1), 3);\n\
     \x20   }\n\
     }\n"
}

/// Write the named fixture files under a fresh temp root.
fn fixture(files: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    for (relative, body) in files {
        let path = tmp.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture path has a parent"))
            .expect("create fixture dirs");
        fs::write(&path, body).expect("write fixture");
    }
    tmp
}

/// Every caller of `symbol` as a `(file relative to root, qualified name)`
/// pair, through the same sqry-db primitive the CLI and MCP surfaces use.
fn callers_of(root: &Path, symbol: &str) -> BTreeSet<(String, String)> {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(RustPlugin::default()));
    let graph = build_unified_graph(root, &plugins, &BuildConfig::default())
        .expect("build_unified_graph succeeds");
    let snapshot = Arc::new(graph.snapshot());
    let db = QueryDb::new(Arc::clone(&snapshot), QueryDbConfig::default());

    let ids = mcp_callers_query(&db, &RelationKey::exact(symbol));
    let mut out = BTreeSet::new();
    for &id in ids.iter() {
        let Some(entry) = snapshot.nodes().get(id) else {
            continue;
        };
        let file = snapshot
            .files()
            .resolve(entry.file)
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_default();
        let name = entry
            .qualified_name
            .and_then(|sid| snapshot.strings().resolve(sid))
            .or_else(|| snapshot.strings().resolve(entry.name))
            .map(|s| s.to_string())
            .unwrap_or_default();
        out.insert((file, name));
    }
    out
}

/// Both files hold a closure on the same line inside a module named `tests`,
/// and both call `shared_helper`. The whole-tree index must report one caller
/// per file, each under the file that actually holds the call.
#[test]
fn closure_callers_are_reported_under_their_own_file() {
    let tmp = fixture(&[(FILE_A, source_a()), (FILE_B, source_b())]);
    let callers = callers_of(tmp.path(), "shared_helper");

    let files: BTreeSet<&str> = callers.iter().map(|(file, _)| file.as_str()).collect();
    assert!(
        files.contains(FILE_A) && files.contains(FILE_B),
        "each file that calls shared_helper must own a caller; got {callers:?}"
    );

    // The two closures share a qualified name, which is exactly why the merge
    // was possible. Both must survive as separate callers.
    let anonymous: Vec<&(String, String)> = callers
        .iter()
        .filter(|(_, name)| name.contains("<anon:closure@"))
        .collect();
    assert_eq!(
        anonymous.len(),
        2,
        "one closure caller per file, not one merged node; got {callers:?}"
    );
    let anonymous_names: BTreeSet<&str> = anonymous.iter().map(|(_, name)| name.as_str()).collect();
    assert_eq!(
        anonymous_names.len(),
        1,
        "precondition: the two closures collide on one qualified name, \
         otherwise this fixture no longer reproduces #850; got {callers:?}"
    );
    let anonymous_files: BTreeSet<&str> = anonymous.iter().map(|(file, _)| file.as_str()).collect();
    assert_eq!(
        anonymous_files,
        BTreeSet::from([FILE_A, FILE_B]),
        "the colliding closures stay in their own files; got {callers:?}"
    );

    // Nothing may be attributed to a file that does not contain the call.
    for (file, name) in &callers {
        let source = fs::read_to_string(tmp.path().join(file)).expect("read fixture back");
        assert!(
            source.contains("shared_helper("),
            "caller {name} is attributed to {file}, which contains no call of shared_helper"
        );
    }
}

/// The whole-tree caller list is the union of the per-crate caller lists.
/// A user should not have to choose between a correct file and a complete
/// list.
#[test]
fn whole_tree_caller_list_equals_the_union_of_the_per_crate_lists() {
    let whole = fixture(&[(FILE_A, source_a()), (FILE_B, source_b())]);
    let only_a = fixture(&[(FILE_A, source_a())]);
    let only_b = fixture(&[(FILE_B, source_b())]);

    let whole_tree = callers_of(whole.path(), "shared_helper");
    let mut union = callers_of(only_a.path(), "shared_helper");
    union.extend(callers_of(only_b.path(), "shared_helper"));

    assert!(
        !union.is_empty(),
        "precondition: a per-crate index finds the closure caller"
    );
    assert_eq!(
        whole_tree, union,
        "whole-tree callers must equal the union of the per-crate callers"
    );
}
