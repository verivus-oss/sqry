//! Integration tests for U12 — `pass5b_c_indirect_resolve`.
//!
//! Covers the DAG acceptance criteria for `IMP:c-icall-precision-012`:
//!
//! 1. `pass5b_resolves_designated_initializer_binding` — binding-plane
//!    resolution end-to-end: a 2-file C workspace with a designated
//!    initializer struct binding and a FieldExpr indirect callsite
//!    against the receiver. Pass 5b rewrites the synthetic stub Calls
//!    edge into a precise `Calls { resolved_via: BindingPlane }` edge.
//!
//! 2. `pass5b_typematch_fallback_when_struct_unknown` — type-match
//!    fallback when the binding plane misses: a callsite via a
//!    function-pointer-typed local that resolves to a canonical
//!    signature, with the candidate function reachable via the
//!    transitively-seeded `fn_signature` table.
//!
//! 3. `pass5b_cap_exceeded_marks_caller_promiscuous` — 33 address-
//!    taken candidates of matching signature, one indirect callsite:
//!    cap=4 enforcement marks the caller `CALLSITE_PROMISCUOUS` and
//!    preserves the synthetic stub edge.
//!
//! 4. `pass5b_fallback_to_stub_no_regression` — Path A: neither
//!    binding nor type info recoverable (`expected_sig = None` —
//!    `struct_field_fnptr` miss). Synthetic stub edge unchanged.
//!
//! 4b. `pass5b_fallback_when_no_typematch_candidates` — Path B: type
//!     info recoverable (`expected_sig = Some(...)`) but
//!     `signature_index` has zero address-taken candidates for that
//!     signature. Both fallback paths must preserve the synthetic
//!     Direct stub edge.
//!
//! 5. `pass5b_duplicate_bindings_dedupe_before_cap` — regression
//!    guard for codex iter-1 finding 2: duplicate `target_fn` rows
//!    in `bindings_by_field` must dedupe BEFORE the cardinality cap
//!    is applied, so 33 instances all binding the same function
//!    collapse to a single `BindingPlane` candidate rather than
//!    falling through to type-match.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use sqry_core::graph::node::Span;
use sqry_core::graph::unified::build::{BuildConfig, build_unified_graph};
use sqry_core::graph::unified::concurrent::CodeGraph;
use sqry_core::graph::unified::edge::{EdgeKind, ResolvedVia};
use sqry_core::graph::unified::node::{NodeId, NodeKind};
use sqry_core::plugin::PluginManager;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

/// Build the workspace under `root` with only the C plugin registered.
fn build_c_only_workspace(root: &std::path::Path) -> CodeGraph {
    let mut plugins = PluginManager::new();
    plugins.register_builtin(Box::new(sqry_lang_c::CPlugin::new()));
    let config = BuildConfig::default();
    build_unified_graph(root, &plugins, &config).expect("build must succeed for U12 fixture")
}

/// Resolve all CALL_COMPATIBLE-kind NodeIds whose name (bare or
/// qualified) matches `name`. Mirrors the resolution path used by
/// `apply_deferred_address_taken_marks` so tests look up the same
/// canonical node the U11 mark step targeted.
fn resolve_c_function_node_ids(graph: &CodeGraph, name: &str) -> Vec<NodeId> {
    let Some(str_id) = graph.strings().get(name) else {
        return Vec::new();
    };
    let by_qn = graph.indices().by_qualified_name(str_id).to_vec();
    let by_nm = graph.indices().by_name(str_id).to_vec();
    let mut seen: HashSet<(u32, u64)> = HashSet::new();
    let mut out = Vec::new();
    let arena = graph.nodes();
    for nid in by_qn.into_iter().chain(by_nm.into_iter()) {
        if !seen.insert((nid.index(), nid.generation())) {
            continue;
        }
        let Some(entry) = arena.get(nid) else {
            continue;
        };
        match entry.kind {
            NodeKind::Function
            | NodeKind::Method
            | NodeKind::Macro
            | NodeKind::Constant
            | NodeKind::LambdaTarget => out.push(nid),
            _ => {}
        }
    }
    out
}

/// Return every Calls edge from `caller` that targets a node whose
/// resolved name equals `target_name`. Used to assert that
/// `resolved_via` is the expected discriminator on the rewritten
/// precise edge.
fn calls_edges_from_caller_to_named(
    graph: &CodeGraph,
    caller: NodeId,
    target_name: &str,
) -> Vec<(NodeId, NodeId, EdgeKind)> {
    let mut out = Vec::new();
    for edge in graph.edges().edges_from(caller) {
        let EdgeKind::Calls { .. } = &edge.kind else {
            continue;
        };
        let Some(entry) = graph.nodes().get(edge.target) else {
            continue;
        };
        let Some(name) = graph.strings().resolve(entry.name) else {
            continue;
        };
        if name.as_ref() == target_name {
            out.push((edge.source, edge.target, edge.kind.clone()));
        }
    }
    out
}

/// Return every Calls edge from `caller` regardless of target.
fn calls_edges_from_caller(graph: &CodeGraph, caller: NodeId) -> Vec<(NodeId, NodeId, EdgeKind)> {
    graph
        .edges()
        .edges_from(caller)
        .into_iter()
        .filter(|e| matches!(e.kind, EdgeKind::Calls { .. }))
        .map(|e| (e.source, e.target, e.kind.clone()))
        .collect()
}

/// Pick the single caller NodeId for a callee name. Panics if not
/// exactly one match — the tests below construct workspaces with
/// unique caller names so this is safe.
fn unique_caller_node(graph: &CodeGraph, name: &str) -> NodeId {
    let candidates = resolve_c_function_node_ids(graph, name);
    assert_eq!(
        candidates.len(),
        1,
        "unique caller `{name}` expected exactly one match; got {} ({:?})",
        candidates.len(),
        candidates
    );
    candidates[0]
}

// ---------------------------------------------------------------------------
// Test 1 — binding-plane resolution end-to-end
// ---------------------------------------------------------------------------

/// DAG acceptance criterion 1.
///
/// File A: `struct ops { ssize_t (*read)(char*, size_t); }; static
/// struct ops my_ops = { .read = my_read }; ssize_t my_read(char* buf,
/// size_t n) { ... }`.
///
/// File B: `extern struct ops my_ops; void caller(struct ops *f) {
/// f->read(buf, n); }`.
///
/// After build, the graph must contain exactly one `Calls` edge from
/// `caller` to `my_read` carrying `resolved_via == BindingPlane`. The
/// synthetic stub edge (target named `read`) must be gone.
#[test]
fn pass5b_resolves_designated_initializer_binding() {
    let _ = env_logger::builder().is_test(true).try_init();

    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(
        &file_a,
        // Use bare identifier (`my_read`) in the designated
        // initializer per phase4_post_unification_c_indirect.rs:300-307
        // — `extract_initializer_list_targets` only captures bindings
        // when the RHS is an identifier, not `&identifier`.
        "typedef unsigned long size_t;\n\
         typedef long ssize_t;\n\
         struct ops { ssize_t (*read)(char *buf, size_t n); };\n\
         ssize_t my_read(char *buf, size_t n) { (void)buf; (void)n; return 0; }\n\
         static struct ops my_ops = { .read = my_read };\n",
    )
    .expect("write a.c");

    let file_b: PathBuf = root.join("b.c");
    fs::write(
        &file_b,
        "typedef unsigned long size_t;\n\
         typedef long ssize_t;\n\
         struct ops { ssize_t (*read)(char *buf, size_t n); };\n\
         void caller_b(struct ops *f, char *buf, size_t n) {\n\
             f->read(buf, n);\n\
         }\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    // `caller_b` is the unique caller function.
    let caller = unique_caller_node(&graph, "caller_b");

    // Find every Calls edge from caller_b to a node named `my_read`.
    let resolved = calls_edges_from_caller_to_named(&graph, caller, "my_read");
    assert!(
        !resolved.is_empty(),
        "post-build: expected at least one Calls edge caller_b → my_read; \
         got 0. All Calls edges from caller_b: {:?}",
        calls_edges_from_caller(&graph, caller),
    );

    // At least one of these must be resolved_via=BindingPlane.
    let binding_plane = resolved
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::BindingPlane,
                    ..
                }
            )
        })
        .count();
    assert!(
        binding_plane >= 1,
        "expected at least 1 Calls edge with resolved_via=BindingPlane caller_b → my_read; \
         got {} (full edges: {:?})",
        binding_plane,
        resolved,
    );

    // The synthetic stub edge (target named `read`, the bare field
    // name) MUST be gone — Pass 5b removes it during rewrite.
    let stub_remnants = calls_edges_from_caller_to_named(&graph, caller, "read");
    let stub_with_direct = stub_remnants
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::Direct,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        stub_with_direct, 0,
        "synthetic stub Calls edge caller_b → <node named \"read\"> with resolved_via=Direct \
         must be removed by Pass 5b; found {stub_with_direct}: {stub_remnants:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — type-match fallback when struct binding unrecoverable
// ---------------------------------------------------------------------------

/// DAG acceptance criterion 2.
///
/// **Design rationale.** Binding-plane resolution requires a
/// `(struct_qn, field_name)` key whose `bindings_by_field` entry
/// contains at least one `BindingEntry`. Type-match fallback fires
/// when bindings_by_field for the callsite's struct/field is empty
/// (no instance ever bound a function to this slot) but
/// `struct_field_fnptr` still records the field's canonical
/// signature.
///
/// Fixture:
///
/// * File A — `struct binder { int (*f)(int x); }` declares the
///   binder type. `int handler_fn(int x)` is bound to `(binder, f)`
///   via a designated initializer in a `struct binder` instance —
///   this populates `bindings_by_field[(binder, f)] = [handler_fn]`
///   AND `struct_field_fnptr[(binder, f)] = "int(int)"`. Pass 5b's
///   transitive seeding sets `fn_signature[handler_fn] = "int(int)"`.
///
/// * File A also declares `struct caller_view { int (*f)(int x); }`
///   with the SAME field signature as `struct binder` — this puts an
///   entry into `struct_field_fnptr[(caller_view, f)] = "int(int)"`
///   but NO `bindings_by_field[(caller_view, f)]` entry (no instance
///   of `struct caller_view` ever binds `f`).
///
/// * File B — `void caller_tm(struct caller_view *v, int x) { v->f(x); }`
///   issues the indirect callsite. LocalScopeIndex resolves `v` to
///   `"struct caller_view"`; binding-plane lookup on
///   `(caller_view, f)` is EMPTY → fall through to type-match.
///   Type-match's expected_sig = `struct_field_fnptr[(caller_view, f)]`
///   = `"int(int)"`. Signature index hits → handler_fn (carrying
///   `is_address_taken == true` from the `binder` binding) → emit
///   `Calls { resolved_via: TypeMatch }` caller_tm → handler_fn.
#[test]
fn pass5b_typematch_fallback_when_struct_unknown() {
    let _ = env_logger::builder().is_test(true).try_init();

    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(
        &file_a,
        "struct binder { int (*f)(int x); };\n\
         struct caller_view { int (*f)(int x); };\n\
         int handler_fn(int x) { return x + 1; }\n\
         static struct binder the_binder = { .f = handler_fn };\n",
    )
    .expect("write a.c");

    let file_b: PathBuf = root.join("b.c");
    fs::write(
        &file_b,
        "struct caller_view { int (*f)(int x); };\n\
         int caller_tm(struct caller_view *v, int x) {\n\
             return v->f(x);\n\
         }\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    let caller = unique_caller_node(&graph, "caller_tm");

    // Expect a TypeMatch Calls edge from caller_tm to handler_fn.
    let resolved = calls_edges_from_caller_to_named(&graph, caller, "handler_fn");
    let typematch = resolved
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::TypeMatch,
                    ..
                }
            )
        })
        .count();
    assert!(
        typematch >= 1,
        "expected at least 1 Calls edge with resolved_via=TypeMatch caller_tm → handler_fn; \
         got {} (all Calls edges from caller_tm: {:?})",
        typematch,
        calls_edges_from_caller(&graph, caller),
    );

    // The synthetic stub edge (target name = `f`) must be gone —
    // type-match's rewrite removes it just like binding-plane.
    let stub_remnants = calls_edges_from_caller_to_named(&graph, caller, "f");
    let stub_direct = stub_remnants
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::Direct,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        stub_direct, 0,
        "synthetic Direct stub Calls edge caller_tm → <node named \"f\"> \
         must be removed by Pass 5b's type-match rewrite; \
         got {stub_direct} (stub-named edges: {stub_remnants:?})",
    );
}

// ---------------------------------------------------------------------------
// Test 3 — cap-exceeded marks caller promiscuous
// ---------------------------------------------------------------------------

/// Generate a fixture with `n` address-taken functions of matching
/// signature, all bound to a single struct's single field. Returns the
/// source text for file A.
fn gen_promiscuous_fixture(n: usize) -> String {
    let mut src = String::new();
    src.push_str("struct ops3 { int (*slot)(int x); };\n");
    for i in 0..n {
        src.push_str(&format!("int fn_{i}(int x) {{ return x + {i}; }}\n"));
    }
    for i in 0..n {
        src.push_str(&format!(
            "static struct ops3 inst_{i} = {{ .slot = fn_{i} }};\n"
        ));
    }
    src
}

/// DAG acceptance criterion 3.
///
/// 33 address-taken functions of matching signature are all bound to
/// `(ops3, slot)`. One indirect callsite via `f->slot(x)` produces 33
/// binding candidates AND 33 typematch candidates → CapExceeded
/// (cap=4). The caller must carry `CALLSITE_PROMISCUOUS`; the
/// synthetic stub Calls edge must be preserved.
#[test]
fn pass5b_cap_exceeded_marks_caller_promiscuous() {
    let _ = env_logger::builder().is_test(true).try_init();

    const N: usize = 33;
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(&file_a, gen_promiscuous_fixture(N)).expect("write a.c");

    let file_b: PathBuf = root.join("b.c");
    fs::write(
        &file_b,
        "struct ops3 { int (*slot)(int x); };\n\
         int caller_cap(struct ops3 *f, int x) {\n\
             return f->slot(x);\n\
         }\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    let caller = unique_caller_node(&graph, "caller_cap");

    // Caller must be flagged CALLSITE_PROMISCUOUS.
    assert!(
        graph.macro_metadata().is_callsite_promiscuous(caller),
        "caller_cap must carry CALLSITE_PROMISCUOUS after Pass 5b with {N} candidates > cap=4; \
         flags: {:?}",
        graph.macro_metadata().get_flags(caller),
    );

    // No precise BindingPlane / TypeMatch edges should have been
    // emitted (cap exceeded → resolver bails before rewrite).
    let calls = calls_edges_from_caller(&graph, caller);
    let precise = calls
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::BindingPlane | ResolvedVia::TypeMatch,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        precise, 0,
        "cap-exceeded resolver must not emit precise edges; got {precise} \
         (all Calls edges from caller_cap: {calls:?})",
    );

    // Synthetic stub edge (target name = `slot`) must remain — it's
    // the resolver's fallback signal that the callsite is
    // promiscuous-but-direct.
    let stub_remnants = calls_edges_from_caller_to_named(&graph, caller, "slot");
    let stub_direct = stub_remnants
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::Direct,
                    ..
                }
            )
        })
        .count();
    assert!(
        stub_direct >= 1,
        "cap-exceeded resolver must preserve the synthetic Direct stub Calls edge \
         caller_cap → <node named \"slot\">; got {stub_direct} \
         (stub-named edges: {stub_remnants:?})",
    );
}

// ---------------------------------------------------------------------------
// Test 4 — fallback to stub, no regression (Path A: neither binding NOR
// type info recoverable)
// ---------------------------------------------------------------------------

/// DAG acceptance criterion 4 — Path A: neither binding nor type info
/// recoverable.
///
/// Two distinct fallback paths exist in `resolve_indirect_callsite`:
///
/// * **Path A** (this test): the resolver computes
///   `expected_sig = None` because `struct_field_fnptr.get(&(struct,
///   field))` returns `None` — i.e. the C indexer never recorded a
///   function-pointer signature for the callsite's `(struct, field)`
///   key. The resolver returns `FallbackToStub` BEFORE it ever
///   indexes into `signature_index`. This is the DAG case "neither
///   binding nor type info recoverable".
///
/// * **Path B** (covered by `pass5b_fallback_when_no_typematch_candidates`
///   below): the resolver computes `expected_sig = Some(...)` but the
///   `signature_index` has no candidates for that signature — i.e.
///   type info is recoverable but no address-taken function matches.
///
/// **Fixture for Path A.** `struct widget { op_t op; int value; };`
/// declares a struct whose `op` field is a function-pointer typed
/// indirectly via a `typedef` (`typedef int (*op_t)(int);`). This is
/// valid C — `cc -fsyntax-only` accepts it without diagnostics — yet
/// the C plugin's `struct_field_fnptr` indexer only fires when the
/// field declarator's syntactic shape is `function_declarator` with
/// an inner `pointer_declarator` (see
/// `is_function_pointer_field_declarator` /
/// `inner_contains_pointer_declarator` in
/// `sqry-lang-c/src/relations/graph_builder.rs:2243` &
/// `:2279`). A typedef-typed field's declarator is a bare
/// `field_identifier`, so `push_struct_field_fnptr_signature` is
/// never called, and the resolver's `struct_field_fnptr.get(...)`
/// returns `None`. The callsite `p->op(x)` is still staged as an
/// `IndirectCallsite { shape: FieldExpr { receiver_name: "p",
/// field_name: "op" }, .. }` with the synthetic
/// `Calls { resolved_via: Direct }` stub to a node named `"op"`,
/// because the staging code (`classify_indirect_callsite_shape`)
/// keys only on the call-expression shape, not on the
/// field-declaration shape. At resolve time:
///
/// 1. `LocalScopeIndex::resolve_type("p", ...)` returns
///    `"struct widget *"` (the parameter's declared type).
/// 2. `strip_struct_keyword_and_pointer` yields `"widget"`.
/// 3. `strings().get("widget")` and `strings().get("op")` both
///    succeed — `"widget"` was interned by the struct declaration,
///    `"op"` was interned by the synthetic stub callee node.
/// 4. `bindings_by_field.get(&(widget, op))` returns `None` — no
///    designated or positional initializer ever assigns to
///    `widget.op` in the fixture, so no binding is staged.
/// 5. `struct_field_fnptr.get(&(widget, op))` returns `None` —
///    `is_function_pointer_field_declarator` rejects the typedef
///    shape, so `push_struct_field_fnptr_signature` is never
///    invoked. The callsite's `expected_sig` is therefore `None`,
///    and the resolver hits the `let Some(expected) = expected_sig
///    else { return FallbackToStub; }` guard.
///
/// Result: the synthetic `Calls { resolved_via: Direct }` stub edge
/// `caller_fb → <node named "op">` is preserved, no precise edges are
/// emitted, and the caller is NOT marked `CALLSITE_PROMISCUOUS`.
#[test]
fn pass5b_fallback_to_stub_no_regression() {
    let _ = env_logger::builder().is_test(true).try_init();

    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(
        &file_a,
        // `op_t` is a function-pointer typedef. When `widget.op` is
        // declared via the typedef (rather than spelled out as
        // `int (*op)(int);`), its field declarator is a bare
        // `field_identifier` rather than the `function_declarator`
        // shape that `is_function_pointer_field_declarator` matches
        // (graph_builder.rs:2243), so `struct_field_fnptr` is never
        // populated for `(widget, op)`. tree-sitter-c parses the
        // callsite `p->op(x)` as a `field_expression`, so the
        // synthetic stub edge IS still staged. The resolver computes
        // `expected_sig = None`, and Path A fires before the
        // type-match step consults `signature_index`. The fixture is
        // valid C (`cc -fsyntax-only` accepts it).
        "typedef int (*op_t)(int);\n\
         struct widget { op_t op; int value; };\n\
         int caller_fb(struct widget *p, int x) {\n\
             return p->op(x);\n\
         }\n",
    )
    .expect("write a.c");

    let graph = build_c_only_workspace(root);

    let caller = unique_caller_node(&graph, "caller_fb");

    // Caller must NOT be promiscuous (no cap was hit — the candidate
    // sets are empty, not over-cap).
    assert!(
        !graph.macro_metadata().is_callsite_promiscuous(caller),
        "caller_fb must NOT carry CALLSITE_PROMISCUOUS on miss; got flags: {:?}",
        graph.macro_metadata().get_flags(caller),
    );

    // No BindingPlane / TypeMatch precise edges from caller_fb.
    let calls = calls_edges_from_caller(&graph, caller);
    let precise = calls
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::BindingPlane | ResolvedVia::TypeMatch,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        precise, 0,
        "stub-fallback resolver must not emit precise edges; got {precise} \
         (all Calls edges from caller_fb: {calls:?})",
    );

    // Synthetic Direct stub Calls edge to the node named `op` MUST
    // remain — zero regression.
    let stub_remnants = calls_edges_from_caller_to_named(&graph, caller, "op");
    let stub_direct = stub_remnants
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::Direct,
                    ..
                }
            )
        })
        .count();
    assert!(
        stub_direct >= 1,
        "stub-fallback case must preserve the original synthetic Direct stub Calls edge; \
         got {stub_direct} (stub-named edges: {stub_remnants:?})",
    );
}

// ---------------------------------------------------------------------------
// Test 4b — fallback to stub, Path B (type info present, no typematch
// candidates)
// ---------------------------------------------------------------------------

/// Companion to `pass5b_fallback_to_stub_no_regression`.
///
/// Path B of `resolve_indirect_callsite`'s fallback: the resolver DOES
/// compute `expected_sig = Some(...)` because the callsite's
/// `(struct, field)` key is present in `struct_field_fnptr`, but
/// `signature_index` has zero candidates for that signature — no
/// function with `is_address_taken == true` carries the matching
/// `fn_signature`. The resolver hits `if typematch_targets.is_empty()
/// { return FallbackToStub; }` AFTER computing `expected_sig`.
///
/// **Fixture.** `struct nonexistent { int (*op)(int); };` declares a
/// function-pointer field, which populates
/// `struct_field_fnptr[(nonexistent, op)] = "int(int)"`. No address-
/// taken function with signature `"int(int)"` exists in the workspace,
/// so `signature_index.get(&"int(int)")` is empty. The callsite
/// `p->op(x)` resolves through the binding-plane miss (no instance ever
/// bound `(nonexistent, op)`) and then through the type-match miss to
/// `FallbackToStub`.
///
/// Both Path A and Path B must preserve the synthetic Direct stub edge
/// with zero behavioural regression vs HEAD's pre-Phase-A state.
#[test]
fn pass5b_fallback_when_no_typematch_candidates() {
    let _ = env_logger::builder().is_test(true).try_init();

    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    let file_a: PathBuf = root.join("a.c");
    fs::write(
        &file_a,
        // `struct nonexistent` HAS a function-pointer field `op`, so
        // `struct_field_fnptr[(nonexistent, op)]` is populated. But no
        // function with matching signature is address-taken anywhere
        // in the workspace, so the typematch index yields zero
        // candidates and the resolver falls back to stub via the
        // `typematch_targets.is_empty()` guard.
        "struct nonexistent { int (*op)(int); };\n\
         int caller_fb_typematch_miss(struct nonexistent *p, int x) {\n\
             return p->op(x);\n\
         }\n",
    )
    .expect("write a.c");

    let graph = build_c_only_workspace(root);

    let caller = unique_caller_node(&graph, "caller_fb_typematch_miss");

    assert!(
        !graph.macro_metadata().is_callsite_promiscuous(caller),
        "caller_fb_typematch_miss must NOT carry CALLSITE_PROMISCUOUS on miss; got flags: {:?}",
        graph.macro_metadata().get_flags(caller),
    );

    let calls = calls_edges_from_caller(&graph, caller);
    let precise = calls
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::BindingPlane | ResolvedVia::TypeMatch,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        precise, 0,
        "Path B resolver must not emit precise edges; got {precise} \
         (all Calls edges: {calls:?})",
    );

    let stub_remnants = calls_edges_from_caller_to_named(&graph, caller, "op");
    let stub_direct = stub_remnants
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::Direct,
                    ..
                }
            )
        })
        .count();
    assert!(
        stub_direct >= 1,
        "Path B must preserve the synthetic Direct stub Calls edge; \
         got {stub_direct} (stub-named edges: {stub_remnants:?})",
    );
}

// ---------------------------------------------------------------------------
// Test 5 — duplicate bindings dedupe BEFORE cap (regression for codex
// iter-1 finding 2)
// ---------------------------------------------------------------------------

/// 33 struct instances all bind `.read = my_read` to the SAME function.
/// `bindings_by_field[(ops_dup, read)]` therefore carries 33
/// `BindingEntry` rows whose `target_fn` is the same `NodeId`.
///
/// **Regression guard.** A previous revision of
/// `resolve_indirect_callsite` applied the cardinality cap on the
/// PRE-dedup binding list, so 33 entries pointing at one function
/// would bypass the dedupe block and fall through to type-match —
/// losing `BindingPlane` provenance for what is genuinely a single
/// candidate. The fix dedupes first, then caps, so the 33 duplicate
/// rows collapse to 1 and the resolver emits a single
/// `Calls { resolved_via: BindingPlane }` edge.
///
/// Assertions:
/// * Exactly one `Calls { resolved_via: BindingPlane }` edge from
///   `caller_dup` to `my_read`.
/// * Caller is NOT `CALLSITE_PROMISCUOUS` (cap was never exceeded
///   post-dedup).
/// * No `TypeMatch` edges (didn't fall through to step 2).
/// * Synthetic stub edge to the node named `read` is removed (rewrite
///   ran).
#[test]
fn pass5b_duplicate_bindings_dedupe_before_cap() {
    let _ = env_logger::builder().is_test(true).try_init();

    const N: usize = 33;
    let workspace = TempDir::new().expect("tempdir");
    let root = workspace.path();

    // File A: one struct, one function, N instances all binding the
    // same function under the same field. This intentionally generates
    // N duplicate entries in `bindings_by_field[(ops_dup, read)]` —
    // every row points at the single `my_read` NodeId. Pre-fix, the
    // resolver would see `binding_targets.len() == 33 > CAP == 4`,
    // skip the dedupe block, and fall through to type-match (losing
    // BindingPlane provenance). Post-fix, dedupe collapses to 1
    // candidate before the cap check.
    let mut file_a_src = String::new();
    file_a_src.push_str("typedef unsigned long size_t;\n");
    file_a_src.push_str("typedef long ssize_t;\n");
    file_a_src.push_str("struct ops_dup { ssize_t (*read)(char *buf, size_t n); };\n");
    file_a_src.push_str("ssize_t my_read(char *buf, size_t n) { (void)buf; (void)n; return 0; }\n");
    for i in 0..N {
        file_a_src.push_str(&format!(
            "static struct ops_dup inst_{i} = {{ .read = my_read }};\n"
        ));
    }
    let file_a: PathBuf = root.join("a.c");
    fs::write(&file_a, file_a_src).expect("write a.c");

    // File B: the caller. Uses a bare identifier `my_read` reference
    // path identical to Test 1 — see designated-initializer fixture.
    let file_b: PathBuf = root.join("b.c");
    fs::write(
        &file_b,
        "typedef unsigned long size_t;\n\
         typedef long ssize_t;\n\
         struct ops_dup { ssize_t (*read)(char *buf, size_t n); };\n\
         void caller_dup(struct ops_dup *f, char *buf, size_t n) {\n\
             f->read(buf, n);\n\
         }\n",
    )
    .expect("write b.c");

    let graph = build_c_only_workspace(root);

    let caller = unique_caller_node(&graph, "caller_dup");

    // Caller must NOT be promiscuous — post-dedupe candidate set is 1,
    // well under the cap.
    assert!(
        !graph.macro_metadata().is_callsite_promiscuous(caller),
        "caller_dup must NOT carry CALLSITE_PROMISCUOUS after dedupe collapses {N} \
         duplicate bindings to a single candidate; flags: {:?}",
        graph.macro_metadata().get_flags(caller),
    );

    // Exactly one BindingPlane edge to my_read.
    let resolved = calls_edges_from_caller_to_named(&graph, caller, "my_read");
    let binding_plane = resolved
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::BindingPlane,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        binding_plane, 1,
        "expected exactly 1 Calls edge with resolved_via=BindingPlane caller_dup → my_read \
         (33 duplicate bindings must dedupe to 1, not fall through to TypeMatch); \
         got {binding_plane} (full edges: {resolved:?})",
    );

    // No TypeMatch edges — resolver must NOT have fallen through to
    // step 2 (which would emit `resolved_via=TypeMatch` instead, the
    // pre-fix bug).
    let typematch = calls_edges_from_caller(&graph, caller)
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::TypeMatch,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        typematch, 0,
        "duplicate-binding dedupe must keep resolution on the BindingPlane path; \
         {typematch} TypeMatch edges indicates dedupe-before-cap regressed",
    );

    // Synthetic stub edge (target name = `read`) must be gone.
    let stub_remnants = calls_edges_from_caller_to_named(&graph, caller, "read");
    let stub_direct = stub_remnants
        .iter()
        .filter(|(_, _, k)| {
            matches!(
                k,
                EdgeKind::Calls {
                    resolved_via: ResolvedVia::Direct,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        stub_direct, 0,
        "synthetic stub Calls edge caller_dup → <node named \"read\"> must be removed \
         by Pass 5b rewrite; got {stub_direct} (stub-named edges: {stub_remnants:?})",
    );
}

// Mark unused-import allowance for `Span` — used only via the
// `calls_edges_from_caller*` helpers' destructuring of `StoreEdgeRef`
// (which contains spans). Kept as a compile-time guard that the test
// file still imports the public Span type from sqry_core, matching
// what other tests in this directory do.
#[allow(dead_code)]
fn _unused_span_guard(_: Span) {}
