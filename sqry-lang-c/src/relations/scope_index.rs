//! Per-file local scope index builder for C source files (DESIGN §4.1).
//!
//! This module hosts the **builder** that constructs a [`LocalScopeIndex`]
//! from a parsed `tree-sitter-c` [`Tree`]. The *storage shape* and
//! *lookup logic* live in `sqry-core`
//! (`sqry_core::graph::unified::storage::c_indirect::LocalScopeIndex`)
//! because:
//!
//! * `sqry-core` hosts the `CIndirectSideTables` container that owns the
//!   per-file `HashMap<FileId, LocalScopeIndex>` (U09).
//! * `pass5b_c_indirect_resolve` (U12), which lives in `sqry-core`, must
//!   call `LocalScopeIndex::resolve_type` to map identifiers to type
//!   tokens; that lookup is therefore a public method on the sqry-core
//!   type.
//! * `sqry-core` does not depend on `tree-sitter-c`, so the **builder**
//!   stays in this plugin where the grammar specifics are appropriate.
//!
//! The builder produces the sqry-core type via
//! [`LocalScopeIndex::from_parts`]; consumers of this module use the
//! [`build_local_scope_index`] free function as the entry point.
//!
//! # Block-scope correctness invariant (closes codex DESIGN-iter-1 gap)
//!
//! The earlier "latest-preceding-declaration" heuristic incorrectly resolved
//! identifiers across block boundaries. Consider:
//!
//! ```c
//! int x = 1;
//! { x = 2; int x = 3; }   // The `x = 2` use must resolve to outer `int`.
//! ```
//!
//! A naive innermost-first walk that ignores the inner declaration's byte
//! offset would resolve `x` (at the assignment) to the shadowing inner
//! `int x = 3` — even though the inner declaration lexically follows the
//! use site. The resolver therefore requires the use site to *lexically
//! precede* the declaration's start byte inside the same scope before that
//! declaration is considered a candidate. See
//! [`LocalScopeIndex::resolve_type`] for the algorithm.
//!
//! # Algorithm overview
//!
//! A **single** depth-first tree-sitter walk per file (PERF-280; this was
//! historically two separate walks plus an O(scopes×decls) offset scan —
//! see the note below). The walk performs scope-arena construction and
//! declaration binding simultaneously:
//!
//! * **Scope arena**: a parented arena of [`ScopeEntry`]s. A scope is
//!   opened on entering — and closed on leaving — every one of these
//!   scope-introducing node kinds: `function_definition` (opened on the
//!   whole-function span so parameters are visible inside the body),
//!   `compound_statement` not directly a function / `for` / `if`-decl body,
//!   `for_statement` (the loop's `init`/`condition`/`update` clauses share a
//!   scope with the body, per C99/C11), and the `if_statement` single-decl
//!   branch (C99/C11 `if (T x = ...) { ... }`).
//!
//! * **Declaration binding**: when the walk reaches a `declaration` /
//!   `parameter_declaration`, the declared name + type token is bound to
//!   the **innermost currently-open scope** — the top of the scope stack.
//!   Because the walk is depth-first, every ancestor scope is open and the
//!   stack top is by construction the innermost scope containing the
//!   declaration, so this is identical to (and replaces) the previous
//!   `innermost_scope_for_offset` linear scan over byte ranges.
//!
//! Lookup walks the scope chain innermost-out and returns the matching
//! declaration whose `decl_span.0 <= use_site_offset`.
//!
//! ## PERF-280: single-pass fusion
//!
//! The earlier implementation ran two full recursive walks (a scope-arena
//! pass then a declaration-binding pass) and, in the second pass, located
//! each declaration's owning scope with `innermost_scope_for_offset` — an
//! `O(num_scopes)` linear scan, i.e. O(scopes×decls) per file. Profiling of
//! `bench_full_build_linux_fs_subset` (verivus-oss/sqry#280) showed the
//! scope-index build was the dominant Phase-A build-time cost (~1.08 ms of
//! ~1.8 ms total Phase-A overhead). Fusing into one walk that binds to the
//! scope-stack top is one fewer traversal and O(1) per declaration. The
//! arena, scope indices, parent pointers, and per-scope declaration order
//! are byte-for-byte identical to the two-pass output (the scope-stack top
//! equals the innermost-by-offset scope during a DFS), so resolution
//! results are unchanged — the unit tests below are the guard.
//!
//! # Type token policy
//!
//! [`LocalScopeIndex`] stores the *source-level* type token verbatim — it
//! does **not** apply width-alias normalisation or typedef chasing. Those
//! are downstream concerns (U07 `signature_builder`). A `typedef int MyInt;`
//! followed by `MyInt y;` resolves `y` to `"MyInt"`, not `"int"`.

use sqry_core::graph::unified::storage::c_indirect::{
    LocalDeclaration, LocalScopeIndex, ScopeEntry,
};
use tree_sitter::{Node, Tree, TreeCursor};

/// Build a per-file [`LocalScopeIndex`] from a parsed `tree-sitter-c`
/// [`Tree`].
///
/// `content` must be the same byte slice that was passed to the parser
/// that produced `tree`. The returned index is owned and independent of
/// the tree's lifetime.
///
/// See module documentation for the single-pass algorithm and the
/// block-scope correctness invariant.
#[must_use]
pub fn build_local_scope_index(tree: &Tree, content: &[u8]) -> LocalScopeIndex {
    let mut builder = Builder {
        scopes: Vec::new(),
        decls_by_scope: Vec::new(),
        scope_stack: Vec::new(),
        content,
    };

    // Open a translation-unit scope spanning the entire file as the
    // outermost scope so that file-scope `typedef` and global variables
    // (which are not the focus of indirect-call resolution but might
    // still be observed) have a home.
    let root = tree.root_node();
    builder.open_scope((root.start_byte(), root.end_byte()), None);

    // Single depth-first walk: open/close scopes AND bind declarations to
    // the innermost open scope (scope-stack top) as they are encountered
    // (PERF-280). See module docs for why this is equivalent to the
    // former two-pass + offset-scan implementation.
    builder.walk(root);

    // The root scope is never popped — it spans the whole file.
    debug_assert_eq!(builder.scope_stack.len(), 1);

    LocalScopeIndex::from_parts(builder.scopes, builder.decls_by_scope)
}

// ---------------------------------------------------------------------------
// Builder — internal single-pass walker
// ---------------------------------------------------------------------------

struct Builder<'a> {
    scopes: Vec<ScopeEntry>,
    decls_by_scope: Vec<Vec<LocalDeclaration>>,
    /// Indices of currently-open scopes, innermost last.
    scope_stack: Vec<usize>,
    content: &'a [u8],
}

impl Builder<'_> {
    fn open_scope(&mut self, span: (usize, usize), explicit_parent: Option<usize>) -> usize {
        let parent = explicit_parent.or_else(|| self.scope_stack.last().copied());
        let idx = self.scopes.len();
        self.scopes.push(ScopeEntry::new(span, parent));
        self.decls_by_scope.push(Vec::new());
        self.scope_stack.push(idx);
        idx
    }

    fn close_scope(&mut self) {
        self.scope_stack.pop();
    }

    /// Single depth-first pass: open scopes for the scope-introducing node
    /// kinds AND bind any declaration to the innermost open scope, then
    /// recurse children in source order, then close the scope on exit.
    ///
    /// Scope opening and declaration binding are disjoint node-kind sets
    /// (a `declaration` / `parameter_declaration` is never one of the
    /// scope-introducing kinds), so the relative order of the two `match`
    /// arms below is immaterial.
    fn walk(&mut self, node: Node) {
        let kind = node.kind();
        let mut opened = false;

        // --- scope open ---
        match kind {
            "function_definition" => {
                // The function body (`compound_statement`) is the scope.
                // Function parameters are inside
                // `function_declarator > parameter_list > parameter_declaration`
                // which lives outside the body in tree-sitter-c, so to
                // make parameters visible inside the body we open the
                // scope on the *whole* function_definition span. The
                // `compound_statement` body's own scope-open is then
                // suppressed because we already opened the outer scope.
                self.open_scope((node.start_byte(), node.end_byte()), None);
                opened = true;
            }
            "compound_statement" => {
                // Only open a scope here if our parent is NOT a
                // function_definition (whose scope we already opened) or
                // an if/for/while statement body (where the surrounding
                // construct already opened the scope to include any
                // initializer).
                if !is_inside_already_opened_scope(node) {
                    self.open_scope((node.start_byte(), node.end_byte()), None);
                    opened = true;
                }
            }
            "for_statement" => {
                // C99/C11: `for (int i = 0; ...; ...) body` — `i` is
                // visible in the condition, update, and body, but not
                // after the loop. Open a scope spanning the entire
                // for_statement.
                self.open_scope((node.start_byte(), node.end_byte()), None);
                opened = true;
            }
            "if_statement" => {
                // C99/C11 single-decl branch: `if (T x = expr) { ... }`.
                // The declaration is visible in the condition AND inside
                // the body. Tree-sitter-c only models a `condition`
                // (parenthesized expression / declaration). When the
                // condition contains a declaration, scope the entire
                // if_statement.
                if if_has_decl_in_condition(node) {
                    self.open_scope((node.start_byte(), node.end_byte()), None);
                    opened = true;
                }
            }
            _ => {}
        }

        // --- declaration binding (to innermost open scope) ---
        match kind {
            "declaration" => {
                self.bind_declaration(node);
            }
            "parameter_declaration" => {
                self.bind_parameter_declaration(node);
            }
            _ => {}
        }

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child);
        }

        if opened {
            self.close_scope();
        }
    }

    fn bind_declaration(&mut self, node: Node) {
        // tree-sitter-c shape:
        //   declaration { type: <type_specifier>, declarator: <declarator>+ }
        // declarator can be: identifier, init_declarator{declarator,value},
        // pointer_declarator, array_declarator, function_declarator, etc.
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let type_token = source_slice(type_node, self.content);

        let mut cursor = node.walk();
        for child in node.children_by_field_name("declarator", &mut cursor) {
            self.bind_one_declarator(child, &type_token, node);
        }
    }

    fn bind_parameter_declaration(&mut self, node: Node) {
        let Some(type_node) = node.child_by_field_name("type") else {
            return;
        };
        let type_token = source_slice(type_node, self.content);
        if let Some(declarator) = node.child_by_field_name("declarator") {
            self.bind_one_declarator(declarator, &type_token, node);
        }
    }

    /// Walk a declarator subtree to extract the bound identifier name,
    /// stripping pointer / array / function declarator wrappers.
    fn bind_one_declarator(&mut self, declarator: Node, type_token: &str, owner: Node) {
        let Some(name) = extract_declarator_name(declarator, self.content) else {
            return;
        };

        let decl_span = (owner.start_byte(), owner.end_byte());
        // The innermost currently-open scope (scope-stack top) is, during
        // a depth-first walk, exactly the innermost scope whose span
        // contains `owner.start_byte()` — see module docs (PERF-280). The
        // file-level scope opened in `build_local_scope_index` guarantees
        // the stack is never empty.
        let scope_idx = *self
            .scope_stack
            .last()
            .expect("the file-level scope is always open");

        self.decls_by_scope[scope_idx].push(LocalDeclaration::new(
            name,
            type_token.to_string(),
            decl_span,
            scope_idx,
        ));
    }
}

// ---------------------------------------------------------------------------
// AST helpers
// ---------------------------------------------------------------------------

/// Slice the source-level text for `node` from `content`. Falls back to an
/// empty string on UTF-8 error rather than panicking — the index is a
/// best-effort side table and the indirect-call resolver already
/// fallback-routes through the synthetic stub on a miss.
fn source_slice(node: Node, content: &[u8]) -> String {
    let start = node.start_byte();
    let end = node.end_byte();
    if start > end || end > content.len() {
        return String::new();
    }
    std::str::from_utf8(&content[start..end])
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Walk a declarator subtree to find the bound identifier. Mirrors the
/// declarator-stripping logic used elsewhere in this crate (see
/// `extract_function_name_from_declarator` in `graph_builder.rs`).
fn extract_declarator_name(node: Node, content: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" | "field_identifier" | "type_identifier" => Some(source_slice(node, content)),
        "init_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            extract_declarator_name(inner, content)
        }
        "pointer_declarator"
        | "array_declarator"
        | "function_declarator"
        | "parenthesized_declarator"
        | "abstract_pointer_declarator"
        | "abstract_array_declarator"
        | "abstract_function_declarator" => {
            // tree-sitter-c gives these declarator kinds a `declarator`
            // field child (sometimes positional). Probe both.
            if let Some(inner) = node.child_by_field_name("declarator") {
                return extract_declarator_name(inner, content);
            }
            // Positional fallback: search children for the first nested
            // declarator-shaped node.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if is_declarator_kind(child.kind())
                    && let Some(n) = extract_declarator_name(child, content)
                {
                    return Some(n);
                }
            }
            None
        }
        _ => None,
    }
}

fn is_declarator_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "field_identifier"
            | "type_identifier"
            | "init_declarator"
            | "pointer_declarator"
            | "array_declarator"
            | "function_declarator"
            | "parenthesized_declarator"
            | "abstract_pointer_declarator"
            | "abstract_array_declarator"
            | "abstract_function_declarator"
    )
}

/// True if `node` (always a `compound_statement`) sits directly inside a
/// scope-introducing parent whose scope we already opened on the parent —
/// avoids double-opening a block scope for the same byte range.
fn is_inside_already_opened_scope(node: Node) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    // Suppression list: parent kinds whose `walk` arm already opens a
    // wider scope covering the child `compound_statement`'s byte range.
    // Listing a parent here avoids double-opening the same block.
    //
    // - `function_definition` — the whole-function span is already open.
    // - `for_statement` — the whole-`for` span (init + body) is open.
    //
    // `while_statement` and `do_statement` are **deliberately absent**:
    // standard C does not permit declarations in their conditions, so
    // `walk` does not open a wider scope at the parent. The
    // compound_statement body therefore MUST open its own scope so
    // declarations inside the loop body do not leak to the enclosing
    // scope after the loop ends. Codex U08 iter-1 caught the prior bug
    // where suppressing here left while/do bodies effectively scope-less.
    matches!(parent.kind(), "function_definition" | "for_statement")
        || (parent.kind() == "if_statement" && if_has_decl_in_condition(parent))
}

/// True if an `if_statement`'s condition contains a C99/C11
/// declaration-in-condition (`if (T x = expr) ...`).
fn if_has_decl_in_condition(if_node: Node) -> bool {
    // tree-sitter-c models the condition under field `condition` whose
    // node-kind is `parenthesized_expression`. Inside that, a declaration
    // child appears for the C99/C11 form.
    let Some(cond) = if_node.child_by_field_name("condition") else {
        return false;
    };
    contains_declaration_descendant(cond)
}

fn contains_declaration_descendant(node: Node) -> bool {
    if node.kind() == "declaration" || node.kind() == "init_declarator" {
        return true;
    }
    let mut cursor: TreeCursor = node.walk();
    for child in node.children(&mut cursor) {
        if contains_declaration_descendant(child) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Tests — TEST:c-icall-precision-020
// ---------------------------------------------------------------------------

#[cfg(test)]
mod scope_index_tests {
    use super::*;
    use tree_sitter::Parser;

    fn parse(src: &str) -> Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_c::LANGUAGE.into())
            .expect("load tree-sitter-c");
        parser.parse(src, None).expect("parse C source")
    }

    fn build(src: &str) -> LocalScopeIndex {
        let tree = parse(src);
        build_local_scope_index(&tree, src.as_bytes())
    }

    /// Helper: byte offset of the Nth occurrence (1-indexed) of `needle`
    /// inside `src`.
    fn nth_offset(src: &str, needle: &str, n: usize) -> usize {
        let mut count = 0;
        let mut search_start = 0;
        while let Some(rel) = src[search_start..].find(needle) {
            count += 1;
            let abs = search_start + rel;
            if count == n {
                return abs;
            }
            search_start = abs + needle.len();
        }
        panic!(
            "needle {:?} occurrence #{} not found in source:\n{}",
            needle, n, src
        );
    }

    /// Test 1 — single function scope: `void f() { int x = 1; x; }`.
    #[test]
    fn single_function_scope_resolves_local_int() {
        let src = "void f() { int x = 1; x; }";
        let idx = build(src);
        let use_off = nth_offset(src, "x", 2); // The bare `x;` use site.
        let ty = idx.resolve_type("x", use_off);
        assert_eq!(ty, Some("int"));
    }

    /// Test 2 — block shadowing basic: inner `float x` shadows outer `int x`.
    #[test]
    fn block_shadowing_basic_inner_use_resolves_to_inner_type() {
        let src = "void f() { int x; { float x; x; } }";
        let idx = build(src);
        // Inner `x;` is the THIRD occurrence of `x` (outer-decl, inner-decl,
        // inner-use).
        let inner_use_off = nth_offset(src, "x", 3);
        let ty = idx.resolve_type("x", inner_use_off);
        assert_eq!(ty, Some("float"));
    }

    /// Test 3 — the bug-closer: a use that *lexically precedes* the
    /// shadowing inner declaration must NOT pick up the inner type.
    ///
    /// `int x = 1; { x = 2; int x = 3; }` — the `x = 2` assignment, at an
    /// offset BEFORE the inner `int x = 3`, must resolve to the outer
    /// `int`, not the shadowing inner declaration.
    #[test]
    fn block_shadowing_lexical_precedence_bug_closer() {
        let src = "void f() { int x = 1; { x = 2; int x = 3; } }";
        let idx = build(src);
        // The `x = 2` assignment's `x` is the SECOND occurrence of `x` in
        // the source (after the outer `int x = 1`).
        let assign_x_off = nth_offset(src, "x", 2);
        let ty = idx.resolve_type("x", assign_x_off);
        assert_eq!(
            ty,
            Some("int"),
            "use at offset {assign_x_off} lexically precedes the shadowing inner \
             `int x = 3` and MUST resolve to the outer `int x = 1`"
        );
    }

    /// Test 4 — for-statement init scope: `i` is visible only inside the
    /// for body, init, condition, and update — not after the loop closes.
    #[test]
    fn for_statement_init_scope_bounds() {
        let src = "void f() { for (int i = 0; i < 10; i++) { i; } i; }";
        let idx = build(src);
        // Inside body: `{ i; }` — should resolve.
        let in_body_off = nth_offset(src, "i;", 1);
        assert_eq!(idx.resolve_type("i", in_body_off), Some("int"));
        // After the loop: `} i; }` — the trailing `i;` should NOT resolve.
        let after_loop_off = nth_offset(src, "i;", 2);
        assert_eq!(idx.resolve_type("i", after_loop_off), None);
    }

    /// Test 5 — if-statement body block scope: a declaration introduced
    /// inside the body of an `if` is visible inside that body but not
    /// after it.
    ///
    /// Note: the DESIGN §4.1 description mentions C99/C11
    /// `if (int x = foo()) { ... }` single-decl form. Standard C
    /// (through C23) does **not** in fact allow declarations in `if`
    /// conditions — only in `for` init clauses. tree-sitter-c emits an
    /// `ERROR` node for `if (int x = ...)`. The
    /// [`Builder::walk`] algorithm still recognises the
    /// declaration-in-condition shape (a `parenthesized_expression`
    /// containing a `declaration` descendant) so a future tree-sitter-c
    /// or C dialect that does parse it would slot in cleanly — but the
    /// only test we can write today exercises the body-block scope of
    /// a plain `if (cond) { decl; use; }`, which is equivalent in
    /// scope semantics.
    #[test]
    fn if_statement_body_block_scope_bounds() {
        let src = "int foo(void); void f() { if (foo()) { int x = 1; x; } x; }";
        let idx = build(src);
        // Inside the if body: the use `x;` resolves.
        let in_body_off = nth_offset(src, "x;", 1);
        assert_eq!(idx.resolve_type("x", in_body_off), Some("int"));
        // After the if-statement: `x` is out of scope.
        let after_if_off = nth_offset(src, "x;", 2);
        assert_eq!(idx.resolve_type("x", after_if_off), None);
    }

    /// Regression: `while_statement` body must open its own block scope.
    /// Codex U08 iter-1 caught a bug where `while_statement` appeared in
    /// the compound_statement-suppression list but no scope was opened
    /// for the while parent, leaving the body scope-less and leaking
    /// declarations to the enclosing scope.
    #[test]
    fn while_statement_body_block_scope_bounds() {
        let src = "int foo(void); void f() { while (foo()) { int x = 1; x; } x; }";
        let idx = build(src);
        // Inside the while body: the use `x;` resolves.
        let in_body_off = nth_offset(src, "x;", 1);
        assert_eq!(idx.resolve_type("x", in_body_off), Some("int"));
        // After the while-statement: `x` is out of scope.
        let after_while_off = nth_offset(src, "x;", 2);
        assert_eq!(idx.resolve_type("x", after_while_off), None);
    }

    /// Regression: `do_statement` body must open its own block scope.
    /// Same bug class as `while_statement_body_block_scope_bounds`.
    #[test]
    fn do_statement_body_block_scope_bounds() {
        let src = "int foo(void); void f() { do { int x = 1; x; } while (foo()); x; }";
        let idx = build(src);
        // Inside the do body: the use `x;` resolves.
        let in_body_off = nth_offset(src, "x;", 1);
        assert_eq!(idx.resolve_type("x", in_body_off), Some("int"));
        // After the do-while statement: `x` is out of scope.
        let after_do_off = nth_offset(src, "x;", 2);
        assert_eq!(idx.resolve_type("x", after_do_off), None);
    }

    /// Test 6 — three-level nesting: a name declared at each level
    /// resolves to the innermost binding visible at the use site.
    #[test]
    fn nested_three_level_scopes_resolve_innermost() {
        let src = "void f() { int a; { char a; { float a; a; } a; } a; }";
        let idx = build(src);
        // Occurrences of the literal `"a;"` in source order:
        //   1: `int a;`     (outer declaration)
        //   2: `char a;`    (mid-level declaration)
        //   3: `float a;`   (inner declaration)
        //   4: `a;`         (innermost use — float)
        //   5: `a;`         (mid-level use — char)
        //   6: `a;`         (outer-level use — int)
        let innermost_use = nth_offset(src, "a;", 4);
        assert_eq!(idx.resolve_type("a", innermost_use), Some("float"));
        let middle_use = nth_offset(src, "a;", 5);
        assert_eq!(idx.resolve_type("a", middle_use), Some("char"));
        let outer_use = nth_offset(src, "a;", 6);
        assert_eq!(idx.resolve_type("a", outer_use), Some("int"));
    }

    /// Test 7 — unresolved name returns None at any offset.
    #[test]
    fn unresolved_returns_none() {
        let src = "void f() { int x = 1; x; }";
        let idx = build(src);
        let use_off = nth_offset(src, "x", 2);
        assert_eq!(idx.resolve_type("undef", use_off), None);
        // Also test an offset outside any explicit scope (start of file).
        assert_eq!(idx.resolve_type("undef", 0), None);
    }

    /// Test 8 — typedef + plain declaration mix. `LocalScopeIndex` does not
    /// chase typedefs (U07's job); `y` should resolve to `"MyInt"`, the
    /// source-level type token verbatim.
    #[test]
    fn typedef_mix_resolves_to_source_type_token() {
        let src = "typedef int MyInt; void f() { MyInt y = 0; y; }";
        let idx = build(src);
        let use_off = nth_offset(src, "y;", 1);
        let ty = idx.resolve_type("y", use_off);
        assert_eq!(
            ty,
            Some("MyInt"),
            "LocalScopeIndex stores source-level type tokens verbatim; \
             typedef resolution lives in U07 signature_builder"
        );
    }
}
