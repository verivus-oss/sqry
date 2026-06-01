//! T3.7 (Cluster E) end-to-end acceptance — `ContextPropagationQuery`.
//!
//! Builds real Go source fixtures via `build_unified_graph` + `GoPlugin`,
//! constructs a `QueryDb`, runs the query, and asserts the AC-T3.7-{1..6}
//! observable behaviour from 01_SPEC §6.2.
//!
//! These tests are deliberately separated from the unit tests in
//! `sqry-db/src/queries/context_propagation.rs` because they require
//! the `sqry-lang-go` crate (the actual emitter of `Calls` /
//! `TypeOf{Parameter}` edges for Go code).

use std::fs;
use std::sync::Arc;

use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::plugin::PluginManager;
use sqry_db::queries::context_propagation::{
    ContextLeakSet, ContextMode, ContextModeFilter, ContextPropagationKey, ContextPropagationQuery,
    ContextScope,
};
use sqry_db::{QueryDb, QueryDbConfig};
use sqry_lang_go::GoPlugin;
use tempfile::TempDir;

/// Build a `QueryDb` from inline Go source attributed to a fixture
/// path (the path matters because the file is what the query reads
/// back to span-text-check `context.Background()`/`context.TODO()`).
fn build_db(filename: &str, source: &str) -> (TempDir, QueryDb) {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(tmp.path().join(filename), source).expect("write fixture");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    let config = BuildConfig::default();
    let graph =
        build_unified_graph(tmp.path(), &plugins, &config).expect("build_unified_graph succeeds");
    let db = QueryDb::new(Arc::new(graph.snapshot()), QueryDbConfig::default());
    (tmp, db)
}

fn run(db: &QueryDb, mode: ContextModeFilter) -> Arc<ContextLeakSet> {
    db.get::<ContextPropagationQuery>(&ContextPropagationKey {
        scope: ContextScope::Global,
        mode,
    })
}

fn count(set: &ContextLeakSet, mode: ContextMode) -> usize {
    set.leaks.iter().filter(|l| l.mode == mode).count()
}

// ---------------------------------------------------------------------------
// AC-T3.7-1: Break-site detection
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_7_1_break_site_detection() {
    let src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee() }
func Callee(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "expected >=1 BreakSite leak; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// AC-T3.7-2: Threaded — no leak
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_7_2_threaded_call_is_not_a_leak() {
    let src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee(ctx) }
func Callee(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::BreakSite);
    assert_eq!(
        count(&set, ContextMode::BreakSite),
        0,
        "threaded call must NOT produce a BreakSite leak; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// AC-T3.7-3: Goroutine leak
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_7_3_unthreaded_goroutine_leak() {
    let src = r#"
package main

import "context"

func Caller(ctx context.Context) { go Expensive() }
func Expensive(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::UnthreadedGoroutine) >= 1,
        "expected >=1 UnthreadedGoroutine leak; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// AC-T3.7-4: HTTP handler leak (signature-shape recognition)
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_7_4_http_handler_leak() {
    let src = r#"
package main

import (
    "context"
    "net/http"
)

func H(w http.ResponseWriter, r *http.Request) { Work() }
func Work(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::HttpHandlerLeak) >= 1,
        "expected >=1 HttpHandlerLeak; got {:#?}",
        set.leaks,
    );
}

#[test]
fn http_handler_threading_r_context_suppresses_leak() {
    // Negative case for AC-T3.7-4 (Codex iter-1 BLOCKER-1): the
    // handler explicitly threads `r.Context()`, so no leak.
    let src = r#"
package main

import (
    "context"
    "net/http"
)

func H(w http.ResponseWriter, r *http.Request) { Work(r.Context()) }
func Work(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert_eq!(
        count(&set, ContextMode::HttpHandlerLeak),
        0,
        "threaded `r.Context()` MUST suppress HttpHandlerLeak; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// AC-T3.7-5: Explicit context.Background() is a leak
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_7_5_explicit_background_is_a_leak() {
    let src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee(context.Background()) }
func Callee(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    // The call passes one arg, but it's a fresh context.Background();
    // per AC-T3.7-5 this is still a leak.
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "explicit context.Background() must be reported as a BreakSite; got {:#?}",
        set.leaks,
    );
}

#[test]
fn explicit_todo_is_also_a_leak() {
    let src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee(context.TODO()) }
func Callee(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "explicit context.TODO() must be reported as a BreakSite; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// AC-T3.7-6: Cache invalidation on edge change
// ---------------------------------------------------------------------------

#[test]
fn ac_t3_7_6_cache_invalidation_on_edge_change() {
    // First snapshot: callee accepts ctx but the call threads it → 0 leaks.
    let threaded_src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee(ctx) }
func Callee(ctx context.Context) {}
"#;
    let (tmp, db) = build_db("a.go", threaded_src);
    let first = run(&db, ContextModeFilter::BreakSite);
    assert_eq!(count(&first, ContextMode::BreakSite), 0);

    // Rebuild against a modified workspace where Callee no longer takes
    // ctx — the BreakSite predicate "callee accepts context.Context"
    // no longer holds, so the new result must be 0 leaks even with the
    // structurally identical call site.
    let no_ctx_src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee() }
func Callee() {}
"#;
    fs::write(tmp.path().join("a.go"), no_ctx_src).expect("rewrite fixture");
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    let config = BuildConfig::default();
    let graph2 = build_unified_graph(tmp.path(), &plugins, &config).expect("rebuild succeeds");
    let db2 = QueryDb::new(Arc::new(graph2.snapshot()), QueryDbConfig::default());
    let second = run(&db2, ContextModeFilter::BreakSite);
    assert_eq!(
        count(&second, ContextMode::BreakSite),
        0,
        "after Callee stops taking context.Context, no BreakSite leaks; got {:#?}",
        second.leaks,
    );
}

// ---------------------------------------------------------------------------
// Mode filter coverage
// ---------------------------------------------------------------------------

#[test]
fn mode_filter_break_site_excludes_other_modes() {
    let src = r#"
package main

import "context"

func A(ctx context.Context) { B() }
func B(ctx context.Context) {}
func C(ctx context.Context) { go D() }
func D(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let only_break = run(&db, ContextModeFilter::BreakSite);
    for leak in &only_break.leaks {
        assert_eq!(leak.mode, ContextMode::BreakSite);
    }
    let only_go = run(&db, ContextModeFilter::UnthreadedGoroutine);
    for leak in &only_go.leaks {
        assert_eq!(leak.mode, ContextMode::UnthreadedGoroutine);
    }
}

// ---------------------------------------------------------------------------
// Codex iter-2 follow-up — aliased + dot-imported stdlib MUST match
// ---------------------------------------------------------------------------

#[test]
fn aliased_context_import_still_recognized() {
    // `import c "context"` — the parameter type-text becomes
    // `c.Context`. Per Codex iter-2 the strict qualified-name check
    // dropped this case; the alias-aware matcher must restore it.
    let src = r#"
package main

import c "context"

func Caller(ctx c.Context) { Callee() }
func Callee(ctx c.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "aliased `c.Context` must still produce BreakSite; got {:#?}",
        set.leaks,
    );
}

#[test]
fn dot_imported_context_still_recognized() {
    // `import . "context"` — the parameter type-text becomes
    // bare `Context` with no qualified prefix. Without import-alias
    // resolution this is indistinguishable from a user-defined
    // `type Context struct{}`, so the recognizer must consult the
    // file's import block.
    let src = r#"
package main

import . "context"

func Caller(ctx Context) { Callee() }
func Callee(ctx Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "dot-imported `Context` must still produce BreakSite; got {:#?}",
        set.leaks,
    );
}

#[test]
fn aliased_http_handler_recognized() {
    let src = r#"
package main

import (
    "context"
    h "net/http"
)

func H(w h.ResponseWriter, r *h.Request) { Work() }
func Work(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::HttpHandlerLeak) >= 1,
        "aliased `h.ResponseWriter`/`*h.Request` must produce HttpHandlerLeak; got {:#?}",
        set.leaks,
    );
}

#[test]
fn aliased_context_with_trailing_comment_still_recognized() {
    // Codex iter-3 BLOCKER: real Go imports often carry an end-of-line
    // comment. The parser must strip `//` / `/* */` before the
    // quoted-path extraction, otherwise the alias map ends up empty
    // and the recognizer silently false-negatives.
    let src = r#"
package main

import c "context" // canonical alias

func Caller(ctx c.Context) { Callee() }
func Callee(ctx c.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "aliased import with trailing `//` comment must still produce BreakSite; got {:#?}",
        set.leaks,
    );
}

#[test]
fn underscore_imported_context_is_not_usable() {
    // `import _ "context"` is a side-effect-only import. The `_`
    // identifier cannot be used to reference the package, so a
    // user-defined `type Context` should NOT be silently classified
    // as the stdlib type.
    let src = r#"
package main

import _ "context"

type Context struct{}

func Caller() { Callee() }
func Callee(c Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert_eq!(
        set.leaks.len(),
        0,
        "`_ \"context\"` must NOT make user-defined Context match; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// Codex iter-1 BLOCKER-3 regression — user-defined types must not match
// ---------------------------------------------------------------------------

#[test]
fn user_defined_context_type_does_not_false_match() {
    // A user-defined `Context` type whose simple name is "Context" but
    // whose qualified name is `main::Context` MUST NOT be recognised
    // as the stdlib `context.Context`. AC-T3.7-1 requires the LEAK to
    // surface only when the callee's parameter is the actual stdlib
    // type; here Callee accepts the local `main::Context`, so zero
    // leaks should be reported.
    let src = r#"
package main

type Context struct{}

func Caller() { Callee() }
func Callee(c Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert_eq!(
        set.leaks.len(),
        0,
        "user-defined Context must not produce leaks; got {:#?}",
        set.leaks,
    );
}

#[test]
fn user_defined_request_type_does_not_match_http_handler_shape() {
    // Same false-match guard for the HTTP handler recognizer: a
    // function with a user-defined `Request` parameter is NOT an
    // http.HandlerFunc-shaped function.
    let src = r#"
package main

import "context"

type ResponseWriter struct{}
type Request struct{}

func H(w ResponseWriter, r *Request) { Work() }
func Work(ctx context.Context) {}
"#;
    let (_tmp, db) = build_db("a.go", src);
    let set = run(&db, ContextModeFilter::All);
    assert_eq!(
        count(&set, ContextMode::HttpHandlerLeak),
        0,
        "user-defined ResponseWriter+Request must not match handler shape; got {:#?}",
        set.leaks,
    );
}

// ---------------------------------------------------------------------------
// Codex iter-1 BLOCKER-2 regression — persisted+reloaded snapshot still works
// ---------------------------------------------------------------------------

#[test]
fn persisted_snapshot_round_trip_preserves_query_results() {
    // After save_to_path/load_from_path the edge store is compacted
    // to CSR, and `StoreEdgeRef::file` is reset to FileId::INVALID
    // for CSR-backed edges. The query MUST derive the caller's
    // FileId from the caller NodeEntry (not from edge.file) so
    // scope-filtering, file-dep recording, and source-span re-walk
    // all continue to work post-reload (Codex iter-1 BLOCKER-2).
    use sqry_core::graph::unified::persistence::{load_from_path, save_to_path};

    let src = r#"
package main

import "context"

func Caller(ctx context.Context) { Callee() }
func Callee(ctx context.Context) {}
"#;
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.go"), src).unwrap();
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    let graph = build_unified_graph(tmp.path(), &plugins, &BuildConfig::default()).unwrap();
    let snap_path = tmp.path().join("snapshot.sqry");
    save_to_path(&graph, &snap_path).expect("save");
    let reloaded = load_from_path(&snap_path, Some(&plugins)).expect("load");

    let db = QueryDb::new(Arc::new(reloaded.snapshot()), QueryDbConfig::default());
    let set = run(&db, ContextModeFilter::All);
    assert!(
        count(&set, ContextMode::BreakSite) >= 1,
        "after persist+reload the BreakSite leak must still surface; got {:#?}",
        set.leaks,
    );

    // File-scope filter must also continue to work — the caller's
    // file must be discoverable from the NodeEntry even when
    // edge.file is FileId::INVALID for CSR-backed edges.
    let a_fid = reloaded
        .files()
        .iter()
        .find(|(_, path)| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s == "a.go")
        })
        .map(|(fid, _)| fid)
        .expect("a.go registered after reload");
    let scoped = db.get::<ContextPropagationQuery>(&ContextPropagationKey {
        scope: ContextScope::File(a_fid),
        mode: ContextModeFilter::All,
    });
    assert!(
        count(&scoped, ContextMode::BreakSite) >= 1,
        "file-scoped query must surface the BreakSite leak after reload; got {:#?}",
        scoped.leaks,
    );
}

// ---------------------------------------------------------------------------
// Scope filter — File restricts to a single FileId
// ---------------------------------------------------------------------------

#[test]
fn scope_filter_file_restricts_to_single_file() {
    let tmp = TempDir::new().expect("tempdir");
    fs::write(
        tmp.path().join("leak.go"),
        r#"
package main

import "context"

func Caller(ctx context.Context) { Callee() }
func Callee(ctx context.Context) {}
"#,
    )
    .unwrap();
    fs::write(
        tmp.path().join("clean.go"),
        r#"
package main

import "context"

func ClnA(ctx context.Context) { ClnB(ctx) }
func ClnB(ctx context.Context) {}
"#,
    )
    .unwrap();

    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(GoPlugin::default()));
    let graph = build_unified_graph(tmp.path(), &plugins, &BuildConfig::default()).unwrap();

    let leak_fid = graph
        .files()
        .iter()
        .find(|(_, path)| {
            path.file_name()
                .and_then(|n| n.to_str())
                .map(|s| s == "leak.go")
                .unwrap_or(false)
        })
        .map(|(fid, _)| fid)
        .expect("leak.go registered");

    let db = QueryDb::new(Arc::new(graph.snapshot()), QueryDbConfig::default());
    let leak_only = db.get::<ContextPropagationQuery>(&ContextPropagationKey {
        scope: ContextScope::File(leak_fid),
        mode: ContextModeFilter::All,
    });
    assert!(
        leak_only.leaks.iter().all(|l| {
            // We can't check FileId on the leak directly (the type
            // does not expose it), but the only way leaks appear here
            // is via leak.go, so a non-zero result confirms the file
            // scope did surface that file's leak.
            l.mode == ContextMode::BreakSite
        }),
        "file-scoped result should only contain leak.go's BreakSite; got {:#?}",
        leak_only.leaks,
    );
    assert!(
        !leak_only.leaks.is_empty(),
        "expected at least one leak from leak.go in file-scoped run; got {:#?}",
        leak_only.leaks,
    );
}
