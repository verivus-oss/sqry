//! U19 — Performance benches for Phase A C indirect-call precision.
//!
//! SPEC §5.2 / DESIGN §14.4 measurement plan. Three benches:
//!
//! 1. `bench_build_local_scope_index` — per-function-body amortized.
//!    Builds the per-file [`LocalScopeIndex`] over a synthetic C function
//!    with ~50 lines of local declarations and field assignments. The
//!    target metric is wall-clock per `build_local_scope_index` call;
//!    this is the load-bearing intra-procedural cost of Phase 1.
//!
//! 2. `bench_pass5b_resolve_synthetic` — 1,000 callsites. Generates a
//!    synthetic C workspace with 1,000 indirect callsites against a
//!    small candidate pool, builds the graph once outside the timing
//!    region, then measures
//!    [`resolve_c_indirect_calls`](sqry_core::graph::unified::build::pass5b_c_indirect::resolve_c_indirect_calls)
//!    using `iter_batched_ref` so each iteration starts from a fresh
//!    clone of the populated graph.
//!
//! 3. `bench_full_build_linux_fs_subset` — end-to-end build of the
//!    Phase A fixture `test-fixtures/c-icall-precision/linux-driver-subset/`.
//!    This is the load-bearing build-time bench per DESIGN §14.4.
//!    The pre-Phase-A baseline JSON committed at
//!    `docs/development/c-semantic-phase-a-icall-precision/measurements/2026-05-14-bench-baseline.json`
//!    is captured against this bench function, and
//!    `scripts/measure/check_phase_a_perf_gate.sh` enforces the +15%
//!    build-time budget against the same mean.

use std::hint::black_box;
use std::path::{Path, PathBuf};

use criterion::{Criterion, criterion_group, criterion_main};

use sqry_core::graph::unified::build::{
    BuildConfig, build_unified_graph, pass5b_c_indirect::resolve_c_indirect_calls,
};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::plugin::PluginManager;
use sqry_lang_c::CPlugin;
use sqry_lang_c::relations::scope_index::build_local_scope_index;
use tempfile::TempDir;
use tree_sitter::{Parser, Tree};

// ---------------------------------------------------------------------------
// Bench 1 — LocalScopeIndex construction
// ---------------------------------------------------------------------------

/// A ~50-line synthetic C function exercising the four scope-introducing
/// node kinds the builder cares about: function body, nested blocks,
/// for-statement init, and if-statement body. Each line introduces a
/// local declaration or a use site so pass 2's declaration-binding walk
/// has plenty to traverse.
const SCOPE_INDEX_FIXTURE: &str = r#"
struct ops {
    int (*read)(char *, unsigned long);
    int (*write)(const char *, unsigned long);
    int (*ioctl)(unsigned int, unsigned long);
};

int handler_a(char *buf, unsigned long n) { (void)buf; (void)n; return 1; }
int handler_b(const char *buf, unsigned long n) { (void)buf; (void)n; return 2; }
int handler_c(unsigned int cmd, unsigned long arg) { (void)cmd; (void)arg; return 3; }

int dispatch(struct ops *o, int kind, char *buf, unsigned long n) {
    int total = 0;
    int sentinel = 0;
    char *cursor = buf;
    unsigned long remaining = n;
    for (int i = 0; i < 8; i++) {
        int local_i = i * 2;
        char *step = cursor + local_i;
        if (kind == 0) {
            int rc = o->read(step, remaining);
            total = total + rc;
        }
        if (kind == 1) {
            int rc = o->write(step, remaining);
            total = total + rc;
        }
        {
            int shadow = 0;
            int rc = o->ioctl((unsigned int)kind, remaining);
            shadow = rc;
            total = total + shadow;
        }
        for (int j = 0; j < 4; j++) {
            int rc = handler_a(step, remaining);
            sentinel = sentinel + rc;
        }
    }
    {
        int tail = 0;
        for (int k = 0; k < 4; k++) {
            int rc = handler_b((const char *)cursor, remaining);
            tail = tail + rc;
        }
        total = total + tail;
    }
    return total + sentinel;
}
"#;

fn parse_c(src: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("load tree-sitter-c");
    parser.parse(src, None).expect("parse C source")
}

fn bench_build_local_scope_index(c: &mut Criterion) {
    let tree = parse_c(SCOPE_INDEX_FIXTURE);
    let content = SCOPE_INDEX_FIXTURE.as_bytes();
    c.bench_function("bench_build_local_scope_index", |b| {
        b.iter(|| {
            let idx = build_local_scope_index(black_box(&tree), black_box(content));
            black_box(idx);
        });
    });
}

// ---------------------------------------------------------------------------
// Bench 2 — pass5b resolution against a 1,000-callsite synthetic graph
// ---------------------------------------------------------------------------

/// Generate a synthetic C workspace large enough to seed ~`callsite_count`
/// indirect callsites and a small candidate pool of address-taken
/// functions. Layout (all in `root`):
///
/// * `ops.h` — defines `struct ops` with one function-pointer field
///   `f` (signature `int (int)`).
/// * `bindings.c` — declares an instance `the_ops` with `.f = handler`
///   plus the address-taken `handler` definition.
/// * `callers_<n>.c` — each file declares one `caller_<n>` function
///   issuing ~32 indirect calls through `struct ops *`.
///
/// The total callsite count is `files * 32`. Files are split so each
/// stays comfortably below tree-sitter / parser limits.
fn write_pass5b_fixture(root: &Path, callsite_count: usize) {
    let common = r#"
struct ops { int (*f)(int x); };
int handler(int x) { return x + 1; }
"#;

    std::fs::write(root.join("ops.h"), "struct ops { int (*f)(int x); };\n").expect("write ops.h");

    std::fs::write(
        root.join("bindings.c"),
        format!("{common}\nstatic struct ops the_ops = {{ .f = handler }};\n",),
    )
    .expect("write bindings.c");

    let calls_per_file: usize = 32;
    let file_count = callsite_count.div_ceil(calls_per_file);
    for fi in 0..file_count {
        let mut body = String::new();
        body.push_str(common);
        body.push_str(&format!(
            "int caller_{fi}(struct ops *o, int x) {{\n    int t = 0;\n",
        ));
        for ci in 0..calls_per_file {
            body.push_str(&format!("    t = t + o->f(x + {ci});\n"));
        }
        body.push_str("    return t;\n}\n");
        std::fs::write(root.join(format!("callers_{fi}.c")), body).expect("write callers_<n>.c");
    }
}

fn build_c_only(root: &Path) -> CodeGraph {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(CPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config)
        .expect("build_unified_graph must succeed for pass5b bench fixture")
}

fn bench_pass5b_resolve_synthetic(c: &mut Criterion) {
    // Build the synthetic workspace ONCE at bench setup. The criterion
    // iter loop clones the graph per iteration and measures only
    // resolve_c_indirect_calls.
    let tmp = TempDir::new().expect("tempdir");
    write_pass5b_fixture(tmp.path(), 1_000);
    let baseline_graph = build_c_only(tmp.path());

    // Sanity: confirm the fixture actually produced indirect callsites.
    let pending = baseline_graph
        .c_indirect_tables()
        .map_or(0, |t| t.pending_callsites.len());
    assert!(
        pending > 0,
        "synthetic fixture must produce at least one IndirectCallsite; got {pending}. \
         (Ensure tree-sitter-c parses the generated workspace and the C plugin \
         instruments FieldExpr callsites.)",
    );

    c.bench_function("bench_pass5b_resolve_synthetic", |b| {
        b.iter_batched_ref(
            || baseline_graph.clone(),
            |graph| {
                let stats = resolve_c_indirect_calls(black_box(graph));
                black_box(stats);
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

// ---------------------------------------------------------------------------
// Bench 3 — end-to-end build against the linux-driver-subset fixture
// ---------------------------------------------------------------------------

/// Resolve the absolute path to the Phase A `linux-driver-subset` fixture.
fn linux_driver_subset_fixture() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .expect("sqry-lang-c has a workspace parent")
        .to_path_buf();
    workspace_root.join("test-fixtures/c-icall-precision/linux-driver-subset")
}

fn bench_full_build_linux_fs_subset(c: &mut Criterion) {
    let fixture = linux_driver_subset_fixture();
    assert!(
        fixture.exists(),
        "Phase A fixture must exist at {} (run `git status` to confirm checkout)",
        fixture.display(),
    );

    c.bench_function("bench_full_build_linux_fs_subset", |b| {
        b.iter(|| {
            let graph = build_c_only(black_box(&fixture));
            black_box(graph);
        });
    });
}

// ---------------------------------------------------------------------------
// criterion harness
// ---------------------------------------------------------------------------

criterion_group!(
    c_indirect_benches,
    bench_build_local_scope_index,
    bench_pass5b_resolve_synthetic,
    bench_full_build_linux_fs_subset
);
criterion_main!(c_indirect_benches);
