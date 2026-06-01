//! T3 Cluster F end-to-end — `cfg:` and `wraps:` planner predicates.
//!
//! Builds real Go fixtures via `build_unified_graph` + `GoPlugin`,
//! runs `parse_query` → `execute_plan`, and asserts the predicates
//! filter to the right nodes (01_SPEC AC-T3.6-9, AC-T3.8-10).

use std::fs;
use std::sync::Arc;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::node::NodeId;
use sqry_core::plugin::PluginManager;
use sqry_db::planner::{execute_plan, parse_query};
use sqry_db::{QueryDb, QueryDbConfig};
use sqry_lang_go::GoPlugin;
use sqry_lang_rust::RustPlugin;
use tempfile::TempDir;

fn build_db(files: &[(&str, &str)]) -> (TempDir, QueryDb) {
    let tmp = TempDir::new().expect("tempdir");
    for (name, body) in files {
        fs::write(tmp.path().join(name), body).expect("write fixture");
    }
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    let graph = build_unified_graph(tmp.path(), &plugins, &BuildConfig::default())
        .expect("build_unified_graph succeeds");
    let db = QueryDb::new(Arc::new(graph.snapshot()), QueryDbConfig::default());
    (tmp, db)
}

fn names_for(db: &QueryDb, query: &str) -> Vec<String> {
    let plan = parse_query(query).expect("parse_query");
    let ids: Vec<NodeId> = execute_plan(&plan, db).to_vec();
    let snap = db.snapshot();
    let mut names: Vec<String> = ids
        .into_iter()
        .filter_map(|id| {
            let entry = snap.get_node(id)?;
            let qualified = entry
                .qualified_name
                .and_then(|sid| snap.strings().resolve(sid));
            qualified
                .as_deref()
                .map(|s| s.to_string())
                .or_else(|| snap.strings().resolve(entry.name).map(|s| s.to_string()))
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

// ---------------------------------------------------------------------------
// AC-T3.8-10 — cfg: planner predicate
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_8_10_cfg_predicate_filters_to_linux_gated_functions() {
    // One Linux-gated file + one plain file. `cfg:linux` must surface
    // only the Linux-gated function.
    let linux_src = "//go:build linux\n\npackage cache\nfunc onlyLinux() {}\n";
    let plain_src = "package cache\nfunc plain() {}\n";
    let (_tmp, db) = build_db(&[("linux.go", linux_src), ("plain.go", plain_src)]);

    let names = names_for(&db, "kind:function cfg:linux");
    assert!(
        names.iter().any(|n| n.ends_with("onlyLinux")),
        "cfg:linux must return the Linux-gated function; got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n.ends_with("plain")),
        "cfg:linux must NOT return the unconditional function; got {names:?}",
    );
}

#[test]
fn cfg_predicate_quoted_form_matches_exact_string_only() {
    // Per 02_DESIGN §5.3.a + §10.4: quoted form is byte-exact and
    // language-specific. `cfg:"linux && amd64"` matches a file whose
    // stored `cfg_condition` equals exactly `"linux && amd64"`, and
    // does NOT match a file whose stored form would only be
    // semantically equivalent (e.g. Rust's
    // `all(target_os = "linux", target_arch = "amd64")`). The two
    // addressing modes (literal vs semantic) are kept independently
    // observable so quoted forms stay byte-precise.
    let src = "//go:build linux && amd64\n\npackage cache\nfunc compound() {}\n";
    let (_tmp, db) = build_db(&[("a.go", src)]);

    let names = names_for(&db, "kind:function cfg:\"linux && amd64\"");
    assert!(
        names.iter().any(|n| n.ends_with("compound")),
        "quoted cfg form must match the byte-exact stored function; got {names:?}",
    );

    // A quoted form whose bytes differ from the stored string must
    // NOT match — even if the difference is just operand order.
    // (`linux && amd64` is the canonicalised Go-native form; Cluster
    // D's `to_condition_string` never produces `amd64 && linux` for
    // the same source, so this is a real false-positive guard.)
    let mismatched = names_for(&db, "kind:function cfg:\"amd64 && linux\"");
    assert!(
        !mismatched.iter().any(|n| n.ends_with("compound")),
        "byte-order-reversed quoted form must NOT match; got {mismatched:?}",
    );
}

#[test]
fn cfg_predicate_bare_form_crosses_languages() {
    // Bare form is the cross-language addressing mode. A stored
    // `"linux && amd64"` matches `cfg:linux` because the canonical
    // AST contains a `linux` Flag. (Set-equality with single-flag
    // matchers reduces to "is the single flag one of the operands?";
    // bare matcher is a single Flag and stored is an All — they are
    // NOT semantically equal in the strict sense, so this test
    // documents the strict-match contract.)
    let src = "//go:build linux && amd64\n\npackage cache\nfunc linuxAmd64Fn() {}\n";
    let (_tmp, db) = build_db(&[("a.go", src)]);
    let names = names_for(&db, "kind:function cfg:linux");
    assert!(
        names.iter().any(|n| n.ends_with("linuxAmd64Fn")),
        "bare `cfg:linux` must match a compound function containing linux; got {names:?}",
    );
}

#[test]
fn cfg_predicate_excludes_files_without_constraint() {
    // Plain file (no //go:build, no filename suffix, no cgo) has
    // `cfg_condition == None`. `cfg:linux` must return empty.
    let src = "package cache\nfunc plain() {}\n";
    let (_tmp, db) = build_db(&[("plain.go", src)]);

    let names = names_for(&db, "kind:function cfg:linux");
    assert!(
        names.is_empty(),
        "plain function must NOT match cfg:linux; got {names:?}",
    );
}

// ---------------------------------------------------------------------------
// AC-T3.6-9 — wraps: planner predicate
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_6_9_wraps_predicate_bare_returns_any_wrap_site() {
    // `wrap` calls fmt.Errorf with %w → Wraps edge emitted.
    let src = r#"
package main

import "fmt"

func wrap(inner error) error { return fmt.Errorf("ctx: %w", inner) }
func plain() {}
"#;
    let (_tmp, db) = build_db(&[("a.go", src)]);
    let names = names_for(&db, "kind:function wraps");
    assert!(
        names.iter().any(|n| n.ends_with("wrap")),
        "bare `wraps` predicate must surface the wrap-emitting function; got {names:?}",
    );
    assert!(
        !names.iter().any(|n| n.ends_with("plain")),
        "bare `wraps` must NOT surface non-wrap functions; got {names:?}",
    );
}

#[test]
fn wraps_predicate_filters_by_kind() {
    let src = r#"
package main

import (
    "errors"
    "fmt"
)

var Sentinel = errors.New("oops")

func check(err error) bool { return errors.Is(err, Sentinel) }
func wrap(err error) error { return fmt.Errorf("ctx: %w", err) }
"#;
    let (_tmp, db) = build_db(&[("a.go", src)]);

    // `wraps:errors_is` should match only `check`, not `wrap`.
    let only_errors_is = names_for(&db, "kind:function wraps:errors_is");
    assert!(
        only_errors_is.iter().any(|n| n.ends_with("check")),
        "wraps:errors_is must surface the errors.Is caller; got {only_errors_is:?}",
    );
    assert!(
        !only_errors_is.iter().any(|n| n.ends_with("wrap")),
        "wraps:errors_is must NOT surface the fmt.Errorf %w caller; got {only_errors_is:?}",
    );

    // `wraps:errorf_verb` should match only `wrap`.
    let only_errorf = names_for(&db, "kind:function wraps:errorf_verb");
    assert!(
        only_errorf.iter().any(|n| n.ends_with("wrap")),
        "wraps:errorf_verb must surface the Errorf caller; got {only_errorf:?}",
    );
    assert!(
        !only_errorf.iter().any(|n| n.ends_with("check")),
        "wraps:errorf_verb must NOT surface the errors.Is caller; got {only_errorf:?}",
    );
}

#[test]
fn wraps_predicate_no_match_for_non_wrap_functions() {
    let src = "package main\nfunc plain() {}\nfunc another() {}\n";
    let (_tmp, db) = build_db(&[("a.go", src)]);
    let names = names_for(&db, "kind:function wraps");
    assert!(
        names.is_empty(),
        "wraps with no Wraps edges in graph must return empty; got {names:?}",
    );
}

// ---------------------------------------------------------------------------
// AND-composition: cfg: + wraps: filters compose
// ---------------------------------------------------------------------------

#[test]
fn cfg_and_wraps_compose() {
    let src = r#"//go:build linux

package main

import "fmt"

func linuxWrap(inner error) error { return fmt.Errorf("ctx: %w", inner) }
"#;
    let (_tmp, db) = build_db(&[("a.go", src)]);
    let names = names_for(&db, "kind:function cfg:linux wraps");
    assert!(
        names.iter().any(|n| n.ends_with("linuxWrap")),
        "cfg:linux AND wraps must surface the linux-gated wrap function; got {names:?}",
    );
}

// ---------------------------------------------------------------------------
// 02_DESIGN §10.4 — cross-language `cfg:` regression (Go + Rust in one index)
// ---------------------------------------------------------------------------
//
// Two source files in one workspace: a Go file `cache_linux.go` with
// a `flushGo` function (cfg_condition stored as Go-native `"linux"`)
// and a Rust file `lib.rs` with a `#[cfg(target_os = "linux")] fn
// flush_rust` function (cfg_condition stored as Rust-functional
// `"target_os = \"linux\""`).
//
// Per the design contract the three planner forms must produce
// language-targeted result sets:
//
//   sqry_query "cfg:linux"                       — BOTH symbols.
//   sqry_query 'cfg:"linux"'                      — Go-only.
//   sqry_query 'cfg:"target_os = \"linux\""'      — Rust-only.

#[test]
fn cross_language_cfg_planner_regression() {
    // The Go fixture uses ONLY a `//go:build linux` line (no filename
    // suffix) so the Cluster D canonicaliser stores exactly `"linux"`
    // — `cache_linux.go` + `//go:build linux` would conjoin both
    // sources into `All([linux, linux])` which `to_condition_string`
    // emits as `"linux && linux"`, breaking the byte-exact quoted
    // matcher (the semantic matcher would still match via dedup).
    let go_src = "//go:build linux\n\npackage main\nfunc flushGo() {}\n";
    let rust_src = "#[cfg(target_os = \"linux\")]\npub fn flush_rust() {}\n";

    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join("cache.go"), go_src).unwrap();
    fs::write(tmp.path().join("lib.rs"), rust_src).unwrap();

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    plugins.register_builtin(Box::new(RustPlugin::default()));
    let graph = build_unified_graph(tmp.path(), &plugins, &BuildConfig::default())
        .expect("build_unified_graph succeeds");
    let db = QueryDb::new(Arc::new(graph.snapshot()), QueryDbConfig::default());

    // 1) Bare semantic matcher crosses languages.
    let both = names_for(&db, "kind:function cfg:linux");
    assert!(
        both.iter().any(|n| n.ends_with("flushGo")),
        "bare `cfg:linux` must surface the Go symbol; got {both:?}",
    );
    assert!(
        both.iter().any(|n| n.ends_with("flush_rust")),
        "bare `cfg:linux` must surface the Rust symbol; got {both:?}",
    );

    // 2) Quoted Go form is byte-exact → Go-only.
    let go_only = names_for(&db, "kind:function cfg:\"linux\"");
    assert!(
        go_only.iter().any(|n| n.ends_with("flushGo")),
        "quoted `cfg:\"linux\"` must surface the Go symbol; got {go_only:?}",
    );
    assert!(
        !go_only.iter().any(|n| n.ends_with("flush_rust")),
        "quoted `cfg:\"linux\"` must NOT surface the Rust symbol; got {go_only:?}",
    );

    // 3) Quoted Rust form is byte-exact → Rust-only.
    let rust_only = names_for(&db, "kind:function cfg:\"target_os = \\\"linux\\\"\"");
    assert!(
        rust_only.iter().any(|n| n.ends_with("flush_rust")),
        "quoted Rust-form must surface the Rust symbol; got {rust_only:?}",
    );
    assert!(
        !rust_only.iter().any(|n| n.ends_with("flushGo")),
        "quoted Rust-form must NOT surface the Go symbol; got {rust_only:?}",
    );
}
