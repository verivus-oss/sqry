//! C indirect-call precision (Phase A, U10) — Phase 1 instrumentation tests.
//!
//! These tests exercise every row of the DESIGN §2.5 / §3.1.1 pattern table
//! against the live `<CGraphBuilder as GraphBuilder>::build_graph` entry. For
//! each pattern, we build a small C translation unit, run `build_graph`, and
//! then inspect the per-file `CIndirectStagingPayload` exposed via
//! `StagingGraph::c_indirect()` to verify the correct staging vectors were
//! populated.
//!
//! ## What is asserted
//!
//! * Per `§2.5` row: the address-taken function name is present in
//!   `pending_address_taken_names` (positive cases) or **absent**
//!   (`nonfunction_taken` negative case).
//! * Indirect callsite capture: `field_expression` and `pointer_expression`
//!   callees produce exactly one `PendingIndirectCallsite` with the
//!   correct caller qualified name and shape.
//! * Binding capture: designated initializer produces a `PendingBinding`
//!   keyed on `(struct_tag, field_name)` with the right `instance_name`
//!   and `target_fn_name`.
//! * Struct field signature capture: function-pointer fields land in
//!   `pending_struct_field_signatures` with a non-empty canonical
//!   signature string.
//!
//! ## What is NOT asserted (out of scope for U10)
//!
//! * Resolution to canonical NodeIds — that lives in U11.
//! * Indirect-call edge rewriting — that lives in U12.
//! * Cardinality cap / promiscuous flag — that lives in U12.

use sqry_core::graph::unified::storage::c_indirect::{BindingSiteKind, IndirectShape};
use sqry_core::graph::{GraphBuilder, unified::StagingGraph};
use sqry_lang_c::relations::CGraphBuilder;
use std::path::Path;
use tree_sitter::Parser;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

fn parse_c(src: &str) -> tree_sitter::Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("load tree-sitter-c");
    parser.parse(src.as_bytes(), None).expect("parse C source")
}

/// Run `CGraphBuilder::build_graph` against `src` and return the populated
/// staging buffer for inspection.
fn build(src: &str) -> StagingGraph {
    let tree = parse_c(src);
    let mut staging = StagingGraph::new();
    let builder = CGraphBuilder::default();
    let file = Path::new("tests/fixtures/c/c_indirect_phase1_synthetic.c");
    builder
        .build_graph(&tree, src.as_bytes(), file, &mut staging)
        .expect("build_graph should succeed");
    staging
}

/// True if the staging payload's `pending_address_taken_names` contains
/// a `StringId` resolving to `name` (possibly multiple times — U10
/// tolerates duplicates and U11 idempotently dedups).
///
/// Per DESIGN §2.5 the field carries staging-local `StringId`s; we
/// resolve each entry via `StagingGraph::resolve_local_string` and
/// compare against `name`.
fn marks_address_taken(staging: &StagingGraph, name: &str) -> bool {
    let Some(payload) = staging.c_indirect() else {
        return false;
    };
    payload
        .pending_address_taken_names
        .iter()
        .any(|id| staging.resolve_local_string(*id) == Some(name))
}

// ---------------------------------------------------------------------------
// DESIGN §2.5 pattern coverage — positive cases (one per row)
// ---------------------------------------------------------------------------

#[test]
fn unary_amp_marks_function_address_taken() {
    let src = "
        void f(void) {}
        void g(void) {
            void (*p)(void);
            p = &f;
        }
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "&f must record `f` in pending_address_taken_names"
    );
}

#[test]
fn argument_pass_marks_function_address_taken() {
    let src = "
        void f(void);
        void caller(void (*cb)(void));
        void use_(void) {
            caller(f);
        }
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "passing `f` as a call argument must record it in \
         pending_address_taken_names"
    );
}

#[test]
fn designated_init_marks_address_taken_and_pushes_binding() {
    let src = "
        struct S { void (*cb)(void); };
        void f(void);
        struct S s = { .cb = f };
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "`.cb = f` must record `f` in pending_address_taken_names"
    );
    let bindings = &staging
        .c_indirect()
        .expect("payload populated")
        .pending_bindings;
    let matches: Vec<_> = bindings
        .iter()
        .filter(|b| {
            b.struct_tag == "S"
                && b.field_name == "cb"
                && b.instance_name == "s"
                && b.target_fn_name == "f"
                && b.site_kind == BindingSiteKind::DesignatedInitializer
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one designated binding (S, cb, s -> f); \
         actual bindings: {:?}",
        bindings
    );
}

#[test]
fn positional_init_marks_address_taken_and_pushes_binding() {
    let src = "
        struct S { void (*cb)(void); };
        void f(void);
        struct S s = { f };
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "positional `{{ f }}` must record `f` in pending_address_taken_names"
    );
    let bindings = &staging
        .c_indirect()
        .expect("payload populated")
        .pending_bindings;
    let matches: Vec<_> = bindings
        .iter()
        .filter(|b| {
            b.struct_tag == "S"
                && b.instance_name == "s"
                && b.target_fn_name == "f"
                && b.site_kind == BindingSiteKind::PositionalInitializer
        })
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected one positional binding (S, _, s -> f); \
         actual bindings: {:?}",
        bindings
    );
}

#[test]
fn field_assign_marks_function_address_taken() {
    let src = "
        struct S { void (*cb)(void); };
        void f(void);
        void use_(struct S* s) {
            s->cb = f;
        }
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "`s->cb = f` must record `f` in pending_address_taken_names"
    );
}

#[test]
fn subscript_assign_marks_function_address_taken() {
    let src = "
        void (*table[1])(void);
        void f(void);
        void use_(void) {
            table[0] = f;
        }
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "`table[0] = f` must record `f` in pending_address_taken_names"
    );
}

#[test]
fn return_function_marks_function_address_taken() {
    let src = "
        void f(void);
        void (*pick(int x))(void) {
            (void)x;
            return f;
        }
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "`return f;` must record `f` in pending_address_taken_names"
    );
}

#[test]
fn init_declarator_marks_function_address_taken() {
    let src = "
        void f(void);
        void (*p)(void) = f;
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "f"),
        "init-declarator `void (*p)(void) = f` must record `f` in \
         pending_address_taken_names"
    );
}

// ---------------------------------------------------------------------------
// DESIGN §2.5 negative case: `&g_int` where g_int is not a function
// ---------------------------------------------------------------------------

#[test]
fn nonfunction_taken_does_not_mark_variable_as_address_taken() {
    let src = "
        int g_int;
        void use_(void) {
            int* p = &g_int;
            (void)p;
        }
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "g_int"),
        "`&g_int` must NOT mark a plain variable as address-taken; \
         pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
}

// ---------------------------------------------------------------------------
// DESIGN §4.2 — indirect callsite capture
// ---------------------------------------------------------------------------

#[test]
fn field_expression_callsite_captures_indirect_call() {
    let src = "
        struct S { void (*cb)(void); };
        void use_(struct S* s) {
            s->cb();
        }
    ";
    let staging = build(src);
    let payload = staging.c_indirect().expect("payload populated");
    let field_calls: Vec<_> = payload
        .pending_indirect_callsites
        .iter()
        .filter(|c| matches!(c.shape, IndirectShape::FieldExpr { .. }))
        .collect();
    assert_eq!(
        field_calls.len(),
        1,
        "expected exactly one FieldExpr indirect callsite; \
         actual = {:?}",
        payload.pending_indirect_callsites
    );
    let callsite = field_calls[0];
    assert_eq!(callsite.caller_qualified_name, "use_");
    let IndirectShape::FieldExpr {
        receiver_name,
        field_name,
    } = &callsite.shape
    else {
        unreachable!("filter above");
    };
    assert_eq!(receiver_name, "s");
    assert_eq!(field_name, "cb");
    // Span sanity: nonzero, within the source.
    assert!(callsite.use_span.0 < callsite.use_span.1);
    assert!(callsite.use_span.1 <= src.len());
}

#[test]
fn pointer_expression_callsite_captures_indirect_call() {
    let src = "
        void use_(void (*p)(void)) {
            (*p)();
        }
    ";
    let staging = build(src);
    let payload = staging.c_indirect().expect("payload populated");
    let pointer_calls: Vec<_> = payload
        .pending_indirect_callsites
        .iter()
        .filter(|c| matches!(c.shape, IndirectShape::PointerExpr { .. }))
        .collect();
    assert_eq!(
        pointer_calls.len(),
        1,
        "expected exactly one PointerExpr indirect callsite; \
         actual = {:?}",
        payload.pending_indirect_callsites
    );
    let callsite = pointer_calls[0];
    assert_eq!(callsite.caller_qualified_name, "use_");
    let IndirectShape::PointerExpr { var_name } = &callsite.shape else {
        unreachable!("filter above");
    };
    assert_eq!(var_name, "p");
}

// ---------------------------------------------------------------------------
// DESIGN §3.2.2 / §3.7 — struct function-pointer field signature capture
// ---------------------------------------------------------------------------

#[test]
fn struct_field_signature_capture_populates_pending_struct_field_signatures() {
    let src = "
        struct S { int (*op)(int, int); };
    ";
    let staging = build(src);
    let payload = staging.c_indirect().expect("payload populated");
    let matches: Vec<_> = payload
        .pending_struct_field_signatures
        .iter()
        .filter(|(tag, field, _)| tag == "S" && field == "op")
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one (S, op, <signature>) entry; \
         actual = {:?}",
        payload.pending_struct_field_signatures
    );
    let (_, _, signature) = matches[0];
    assert!(
        !signature.is_empty(),
        "function-pointer field signature must be non-empty for `int (*op)(int, int)`"
    );
    // The DESIGN §3.1 canonical grammar renders this as either
    // `"int|int,int"` or the legacy `"int(int,int)"` shape produced by
    // U07's builder; both are non-empty signatures that downstream
    // U12 can match against. Spot-check that an `int` token appears.
    assert!(
        signature.contains("int"),
        "expected `int` token to appear in signature: {signature}"
    );
}

// ---------------------------------------------------------------------------
// DESIGN §2.6 — type-permits guards (negative cases)
//
// Each test below exercises one of the four guarded rows in DESIGN §2.6
// (positional initializer, field-assign, subscript-assign,
// init_declarator RHS) and proves that when the destination type is NOT
// a function pointer, the classifier does NOT mark the cited identifier
// as address-taken. The matching positive case lives among the tests
// above (in some cases — e.g. positional-mixed — we add a tightly paired
// positive proof inline so the two sit next to each other).
// ---------------------------------------------------------------------------

#[test]
fn positional_init_mixed_slot_marks_only_fnptr_field() {
    // DESIGN §2.6 row 4 — positional init guard, positive paired half.
    //
    // `struct S { int x; void(*cb)(); };` with `struct S s = { 1, g };`
    // initialises field `cb` from `g`. The classifier's positional-init
    // arm (pattern 4) must mark `g` because the slot at field-index 1
    // is fnptr. `f` does NOT appear and must NOT be marked.
    //
    // Pairs with `positional_init_non_fnptr_struct_slot_does_not_mark`
    // below, which exercises the *rejecting* half of the same row.
    let src = "
        void f(void);
        void g(void);
        struct S { int x; void (*cb)(void); };
        struct S s = { 1, g };
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "g"),
        "`g` must be marked when it lands on a fnptr slot in a struct \
         positional initializer; pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
    assert!(
        !marks_address_taken(&staging, "f"),
        "`f` does not appear in the initializer and must not be marked"
    );
}

#[test]
fn positional_init_non_fnptr_struct_slot_does_not_mark() {
    // DESIGN §2.6 row 4 — positional init guard, negative case.
    //
    // `struct S { int x; };` has a single `int` field. The block-local
    // initializer `struct S s = { f };` would, *without the guard*,
    // trip the classifier's `initializer_list > identifier` arm and
    // mark `f`. With the guard, the lookup is:
    //   `s` → struct_tag `S` (via var_struct_tag)
    //   `(S, slot 0)` → `(field_name="x", is_fnptr=false)`
    // The slot is not fnptr, so the classifier must NOT mark `f`.
    //
    // The declaration is placed inside a function body so the
    // top-level extractor `extract_initializer_list_targets` does not
    // fire (it is gated to `is_top_level_declaration`), isolating the
    // classifier arm as the only path that could possibly mark `f`.
    let src = "
        void f(void);
        struct S { int x; };
        void use_(void) {
            struct S s = { f };
            (void)s;
        }
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "f"),
        "`struct S s = {{ f }}` (where `S` has only an `int` field) \
         must NOT mark `f` — the slot is not a function pointer; \
         pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
}

#[test]
fn field_assign_non_fnptr_field_does_not_mark() {
    // DESIGN §2.6 row 5 — field_expression assignment guard, negative.
    //
    // `struct S { int x; };` has a single `int` field. The assignment
    // `s->x = f` would, *without the guard*, trip the classifier's
    // `assignment_expression { field_expression = identifier }` arm
    // and mark `f`. With the guard, the lookup is:
    //   `s` → struct_tag `S` (via var_struct_tag — populated for the
    //   `struct S *s` function parameter)
    //   `(S, "x")` → `is_fnptr = false`
    // The destination is not fnptr, so the classifier must NOT mark.
    let src = "
        void f(void);
        struct S { int x; };
        void use_(struct S* s) {
            s->x = f;
        }
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "f"),
        "`s->x = f` must NOT mark `f` — field `x` is not a function \
         pointer; pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
}

#[test]
fn subscript_assign_non_fnptr_array_does_not_mark() {
    // DESIGN §2.6 row 6 — subscript_expression assignment guard, neg.
    //
    // `int arr[1];` declares an `int` array. The assignment
    // `arr[0] = f` would, *without the guard*, trip the classifier's
    // `assignment_expression { subscript_expression = identifier }`
    // arm and mark `f`. With the guard, the lookup is:
    //   `arr` ∉ var_fnptr_array (because the declarator chain has no
    //   `pointer_declarator > function_declarator`)
    // The destination is not a fnptr array, so the classifier must NOT
    // mark.
    let src = "
        void f(void);
        int arr[1];
        void use_(void) {
            arr[0] = f;
        }
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "f"),
        "`arr[0] = f` must NOT mark `f` — `arr` is `int[]`, not a \
         function-pointer array; pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
}

#[test]
fn init_declarator_non_fnptr_does_not_mark() {
    // DESIGN §2.6 row 8 — init_declarator RHS guard, negative.
    //
    // `int x = f;` declares an `int`, NOT a function pointer. The
    // init_declarator's declarator chain is bare `identifier`, with no
    // `pointer_declarator > function_declarator` shape. Without the
    // guard, pattern 7 (`init_declarator { value: identifier }`) would
    // mark `f`. With the guard, `init_declarator_is_fnptr` returns
    // false (declarator chain has no fnptr shape) and `f` is not
    // marked via this arm.
    //
    // Note: pattern 1 (`&fn`) is *unguarded*, so we deliberately use a
    // bare `f` (no `&`) to isolate the row-8 guard. The init is also
    // placed inside a function body so it doesn't intersect any
    // top-level-decl side-paths.
    let src = "
        void f(void);
        void use_(void) {
            int x = f;
            (void)x;
        }
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "f"),
        "`int x = f` must NOT mark `f` — `x` is `int`, not a function \
         pointer; pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
}

// ---------------------------------------------------------------------------
// DESIGN §2.6 row 4 — top-level (file-scope) positional initializer guard
// ---------------------------------------------------------------------------
//
// Codex iter-2 found that the iter-2 negative test
// `positional_init_non_fnptr_struct_slot_does_not_mark` placed the
// declaration inside a function body, which sidesteps the legacy
// `handle_variable_declaration` → `extract_designated_initializer_targets`
// → `extract_initializer_list_targets` path. That legacy path is gated to
// `is_top_level_declaration` and used to mark every bare positional
// identifier as address-taken unconditionally.
//
// The fix (U10 iter-3) makes `classify_address_taken_sites` the single
// source of truth for address-taken marks; the legacy path no longer
// pushes marks. The two tests below exercise the legacy path at file
// scope and assert:
//   * non-fnptr top-level slots do NOT mark a function identifier; and
//   * fnptr top-level slots still mark and bind correctly.
//
// Citation: docs/reviews/c-semantic-phase-a-icall-precision/IMPL/
//           U10-phase1-instrument/codex-iter-2.md (BLOCKER finding).

#[test]
fn top_level_positional_init_non_fnptr_does_not_mark() {
    // File-scope `struct S { int x; int y; }; struct S s = { 1, 2 };` —
    // no function identifier appears, so nothing should be marked. This
    // is the trivial baseline.
    let src = "
        void f(void);
        struct S { int x; int y; };
        struct S s = { 1, 2 };
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "f"),
        "`f` does not appear in the initializer and must not be marked; \
         pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
}

#[test]
fn top_level_positional_init_non_fnptr_slot_with_function_name_does_not_mark() {
    // DESIGN §2.6 row 4 — top-level positional init guard, negative.
    //
    // Same shape as the block-local negative test
    // (`positional_init_non_fnptr_struct_slot_does_not_mark`) but the
    // declaration is at FILE scope, so it flows through
    // `handle_variable_declaration` → `extract_designated_initializer_targets`
    // → `extract_initializer_list_targets`. The pre-fix legacy path
    // unconditionally marked `f` here. The fix removes the legacy mark;
    // the only path that could mark `f` is
    // `classify_address_taken_sites`'s positional arm, which is guarded
    // by `positional_init_slot_is_fnptr`. The slot is `int x`, not a
    // function pointer, so no mark should appear.
    let src = "
        void f(void);
        struct S { int x; };
        struct S s = { f };
    ";
    let staging = build(src);
    assert!(
        !marks_address_taken(&staging, "f"),
        "top-level `struct S s = {{ f }}` (where `S` has only an `int` \
         field) must NOT mark `f` — the slot is not a function pointer; \
         pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
    // The legacy path also used to push a `(S, <positional>, s -> f)`
    // binding here. The fix gates that push by the same fnptr-slot
    // predicate, so the bindings vector must not contain such an entry.
    let bindings = staging
        .c_indirect()
        .map(|p| p.pending_bindings.clone())
        .unwrap_or_default();
    let leaked: Vec<_> = bindings
        .iter()
        .filter(|b| b.struct_tag == "S" && b.target_fn_name == "f")
        .collect();
    assert!(
        leaked.is_empty(),
        "non-fnptr top-level positional slot must not push a binding; \
         leaked bindings: {:?}",
        leaked
    );
}

#[test]
fn top_level_positional_init_mixed_slot_marks_only_fnptr_field() {
    // DESIGN §2.6 row 4 — top-level positional init guard, positive paired.
    //
    // File-scope companion of
    // `positional_init_mixed_slot_marks_only_fnptr_field`. `struct S` has
    // an `int` slot and a `void(*)(void)` slot; positional initializer
    // `{ 1, g }` assigns `1` to the int slot and `g` to the fnptr slot.
    // The classifier's positional arm must mark `g` (slot 1 is fnptr)
    // and must NOT mark `f` (which does not appear). Additionally, the
    // legacy binding-push path must produce exactly one binding for the
    // fnptr slot (`(S, <positional>, s -> g)`) and no spurious binding
    // for the int slot.
    let src = "
        void f(void);
        void g(void);
        struct S { int x; void (*cb)(void); };
        struct S s = { 1, g };
    ";
    let staging = build(src);
    assert!(
        marks_address_taken(&staging, "g"),
        "top-level `g` in fnptr slot 1 must be marked; \
         pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
    assert!(
        !marks_address_taken(&staging, "f"),
        "`f` does not appear in the initializer and must not be marked; \
         pending_address_taken_names = {:?}",
        staging.c_indirect().map(|p| &p.pending_address_taken_names)
    );
    let bindings = staging
        .c_indirect()
        .map(|p| p.pending_bindings.clone())
        .unwrap_or_default();
    let positional_g: Vec<_> = bindings
        .iter()
        .filter(|b| {
            b.struct_tag == "S"
                && b.instance_name == "s"
                && b.target_fn_name == "g"
                && b.site_kind == BindingSiteKind::PositionalInitializer
        })
        .collect();
    assert_eq!(
        positional_g.len(),
        1,
        "expected exactly one positional binding (S, _, s -> g) for the \
         fnptr slot; actual bindings: {:?}",
        bindings
    );
}
