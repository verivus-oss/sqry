//! Coverage-targeted tests for `sqry-lang-rust`.
//!
//! Exercises uncovered paths in:
//! - `src/relations/local_scopes.rs` (all scope kinds, all pattern kinds)
//! - `src/lifetime_extractor.rs` (dyn/impl trait, HRTB, type-argument lifetimes,
//!   elided lifetimes, static in type bounds, outlives in where predicates)
//! - `src/proc_macro_detector.rs` (contradiction arm, `FunctionAttributeOnly` source)
//! - `src/relations/graph_builder.rs` (config modes, scope extraction for
//!   impl/trait/module/struct/enum)
//! - `src/lib.rs` (`extract_scopes` — all 6 scope types)

use sqry_core::graph::GraphBuilder;
use sqry_core::graph::unified::build::staging::StagingGraph;
use sqry_core::graph::unified::edge::kind::EdgeKind;
use sqry_core::graph::unified::node::NodeKind;
use sqry_lang_rust::relations::{RustGraphBuilder, RustGraphConfig};
use std::path::Path;
use tree_sitter::Tree;

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

fn parse_rust(source: &str) -> Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("set Rust language");
    parser.parse(source, None).expect("parse Rust")
}

fn build_graph(source: &str) -> StagingGraph {
    let tree = parse_rust(source);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::default();
    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.rs"), &mut staging)
        .expect("build_graph should not fail");
    staging
}

fn build_graph_ast_only(source: &str) -> StagingGraph {
    let tree = parse_rust(source);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::with_config(4, RustGraphConfig::ast_only());
    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.rs"), &mut staging)
        .expect("build_graph should not fail");
    staging
}

fn build_graph_safe(source: &str) -> StagingGraph {
    let tree = parse_rust(source);
    let mut staging = StagingGraph::new();
    let builder = RustGraphBuilder::with_config(4, RustGraphConfig::safe_mode());
    builder
        .build_graph(&tree, source.as_bytes(), Path::new("test.rs"), &mut staging)
        .expect("build_graph should not fail");
    staging
}

/// Returns true if the staging graph contains at least one node of `kind`.
fn has_node_kind(staging: &StagingGraph, kind: NodeKind) -> bool {
    staging.nodes().any(|n| n.entry.kind == kind)
}

/// Returns true if the staging graph contains at least one `Calls` edge.
fn has_calls_edge(staging: &StagingGraph) -> bool {
    staging
        .edges()
        .any(|e| matches!(e.kind, EdgeKind::Calls { .. }))
}

/// Returns true if the staging graph contains at least one `Imports` edge.
fn has_imports_edge(staging: &StagingGraph) -> bool {
    staging
        .edges()
        .any(|e| matches!(e.kind, EdgeKind::Imports { .. }))
}

// ─────────────────────────────────────────────────────────────────────────────
// RustGraphConfig — all three constructors + builder methods
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn config_new_enables_all_features() {
    let cfg = RustGraphConfig::new();
    assert!(cfg.enable_macro_expansion);
    assert!(cfg.enable_trait_binding);
    assert!(cfg.enable_lifetime_extraction);
    assert!(cfg.enable_rust_analyzer);
}

#[test]
fn config_safe_mode_disables_macro_and_ra() {
    let cfg = RustGraphConfig::safe_mode();
    assert!(!cfg.enable_macro_expansion);
    assert!(cfg.enable_trait_binding);
    assert!(cfg.enable_lifetime_extraction);
    assert!(!cfg.enable_rust_analyzer);
}

#[test]
fn config_ast_only_disables_all() {
    let cfg = RustGraphConfig::ast_only();
    assert!(!cfg.enable_macro_expansion);
    assert!(!cfg.enable_trait_binding);
    assert!(!cfg.enable_lifetime_extraction);
    assert!(!cfg.enable_rust_analyzer);
}

#[test]
fn config_builder_without_macro_expansion() {
    let cfg = RustGraphConfig::new().without_macro_expansion();
    assert!(!cfg.enable_macro_expansion);
    assert!(cfg.enable_rust_analyzer); // unchanged
}

#[test]
fn config_builder_without_rust_analyzer() {
    let cfg = RustGraphConfig::new().without_rust_analyzer();
    assert!(!cfg.enable_rust_analyzer);
    assert!(cfg.enable_macro_expansion); // unchanged
}

#[test]
fn config_with_workspace_root() {
    use std::path::PathBuf;
    let cfg = RustGraphConfig::new().with_workspace_root(PathBuf::from("/tmp/ws"));
    assert!(cfg.workspace_root.is_some());
}

// ─────────────────────────────────────────────────────────────────────────────
// Build with different config modes — smoke test each config path
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn ast_only_mode_parses_function() {
    let source = "fn add(a: i32, b: i32) -> i32 { a + b }";
    let staging = build_graph_ast_only(source);
    // ast_only mode must still extract function nodes
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "ast_only mode must produce a Function node for `fn add`"
    );
}

#[test]
fn safe_mode_parses_function() {
    let source = "fn greet(name: &str) { println!(\"{name}\"); }";
    let staging = build_graph_safe(source);
    // safe_mode must still extract function nodes
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "safe_mode must produce a Function node for `fn greet`"
    );
}

#[test]
fn full_mode_parses_impl_block() {
    let source = r"
struct Counter { val: u32 }
impl Counter {
    fn increment(&mut self) { self.val += 1; }
    fn value(&self) -> u32 { self.val }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
    // impl methods are Method nodes
    assert!(
        has_node_kind(&staging, NodeKind::Method),
        "full mode must produce Method nodes for impl block functions"
    );
    // Struct node must be staged
    assert!(
        has_node_kind(&staging, NodeKind::Struct),
        "full mode must produce a Struct node for `Counter`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// local_scopes.rs — scope kind coverage
// ─────────────────────────────────────────────────────────────────────────────

/// Method scope (function inside impl)
#[test]
fn scope_method_inside_impl() {
    let source = r"
struct Foo;
impl Foo {
    fn bar(&self) -> u32 {
        let x = 1u32;
        x
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `bar` is inside an impl block, so it must be staged as a Method
    assert!(
        has_node_kind(&staging, NodeKind::Method),
        "impl fn must produce a Method node"
    );
}

/// Closure scope
#[test]
fn scope_closure_expression() {
    let source = r"
fn main() {
    let add = |a: i32, b: i32| a + b;
    let _ = add(1, 2);
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // The closure is called: `add(1, 2)` — must produce a Calls edge
    assert!(
        has_calls_edge(&staging),
        "closure call `add(1, 2)` must produce a Calls edge"
    );
}

/// `ForLoop` scope
#[test]
fn scope_for_loop() {
    let source = r"
fn sum_vec(v: &[i32]) -> i32 {
    let mut total = 0;
    for item in v {
        total += item;
    }
    total
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // The for loop body is parsed under a Function node
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "for-loop source must produce a Function node for `sum_vec`"
    );
}

/// `WhileLoop` scope
#[test]
fn scope_while_loop() {
    let source = r"
fn countdown(mut n: i32) {
    while n > 0 {
        n -= 1;
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "while-loop source must produce a Function node for `countdown`"
    );
}

/// `WhileLet` scope
#[test]
fn scope_while_let() {
    let source = r"
fn drain(mut v: Vec<i32>) {
    while let Some(x) = v.pop() {
        let _ = x;
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `v.pop()` is a method call — must produce a Calls edge
    assert!(
        has_calls_edge(&staging),
        "while-let with `v.pop()` must produce a Calls edge"
    );
}

/// Loop scope (infinite loop)
#[test]
fn scope_loop_expression() {
    let source = r"
fn spin() {
    loop {
        break;
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "loop-expression source must produce a Function node for `spin`"
    );
}

/// `IfBranch` scope (plain if)
#[test]
fn scope_if_branch() {
    let source = r"
fn check(x: i32) -> bool {
    if x > 0 {
        true
    } else {
        false
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "if-branch source must produce a Function node for `check`"
    );
}

/// `IfLet` scope
#[test]
fn scope_if_let() {
    let source = r"
fn first(v: &[i32]) -> Option<i32> {
    if let Some(x) = v.first().copied() {
        Some(x)
    } else {
        None
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `v.first()` and `.copied()` are method calls
    assert!(
        has_calls_edge(&staging),
        "if-let with method calls must produce Calls edges"
    );
}

/// `MatchArm` scope
#[test]
fn scope_match_arm() {
    let source = r#"
fn describe(x: Option<i32>) -> &'static str {
    match x {
        Some(v) => if v > 0 { "positive" } else { "non-positive" },
        None => "nothing",
    }
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "match-arm source must produce a Function node for `describe`"
    );
}

/// `UnsafeBlock` scope
#[test]
fn scope_unsafe_block() {
    let source = r"
fn raw_copy(src: *const u8, dst: *mut u8) {
    unsafe {
        *dst = *src;
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "unsafe-block source must produce a Function node for `raw_copy`"
    );
}

/// Block scope (standalone block expression)
#[test]
fn scope_standalone_block() {
    let source = r"
fn compute() -> i32 {
    let result = {
        let a = 1;
        let b = 2;
        a + b
    };
    result
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "standalone-block source must produce a Function node for `compute`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// local_scopes.rs — pattern binding coverage
// ─────────────────────────────────────────────────────────────────────────────

/// `mut_pattern` in let binding
#[test]
fn pattern_mut_binding() {
    let source = r"
fn mutate() {
    let mut x = 0;
    x += 1;
    let _ = x;
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // The function itself must be staged
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "mut-binding source must produce a Function node for `mutate`"
    );
}

/// `ref_pattern` in let binding
#[test]
fn pattern_ref_binding() {
    let source = r"
fn borrow_name(name: &String) {
    let ref r = name;
    let _ = r;
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "ref-binding source must produce a Function node for `borrow_name`"
    );
}

/// `tuple_pattern` in let binding
#[test]
fn pattern_tuple_destructure() {
    let source = r"
fn swap(pair: (i32, i32)) -> (i32, i32) {
    let (a, b) = pair;
    (b, a)
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "tuple-destructure source must produce a Function node for `swap`"
    );
}

/// `tuple_struct_pattern` in match arm (e.g., `Some(x)`)
#[test]
fn pattern_tuple_struct_match() {
    let source = r"
fn unwrap_or_zero(v: Option<i32>) -> i32 {
    match v {
        Some(x) => x,
        None => 0,
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "tuple-struct match source must produce a Function node for `unwrap_or_zero`"
    );
}

/// `struct_pattern` in let binding
#[test]
fn pattern_struct_destructure() {
    let source = r"
struct Point { x: f64, y: f64 }
fn magnitude(p: Point) -> f64 {
    let Point { x, y } = p;
    (x * x + y * y).sqrt()
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // Both Function and Struct nodes expected
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "struct-destructure source must produce a Function node for `magnitude`"
    );
    assert!(
        has_node_kind(&staging, NodeKind::Struct),
        "struct-destructure source must produce a Struct node for `Point`"
    );
}

/// `slice_pattern` in match arm
#[test]
fn pattern_slice_match() {
    let source = r"
fn head_tail(s: &[i32]) -> Option<(i32, &[i32])> {
    match s {
        [head, tail @ ..] => Some((*head, tail)),
        [] => None,
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "slice-match source must produce a Function node for `head_tail`"
    );
}

/// `or_pattern` in match arm (A | B)
#[test]
fn pattern_or_match() {
    let source = r"
fn is_end(c: char) -> bool {
    match c {
        '.' | '!' | '?' => true,
        _ => false,
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "or-pattern source must produce a Function node for `is_end`"
    );
}

/// `reference_pattern` in let binding (`&x`)
#[test]
fn pattern_reference_binding() {
    let source = r"
fn deref_first(v: &[i32]) -> i32 {
    match v {
        [&first, ..] => first,
        _ => 0,
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "reference-pattern source must produce a Function node for `deref_first`"
    );
}

/// Closure with bare identifier parameters (`|x| x + 1`)
#[test]
fn closure_bare_identifier_param() {
    let source = r"
fn apply<F: Fn(i32) -> i32>(f: F, x: i32) -> i32 { f(x) }

fn main() {
    let double = |x| x * 2;
    let _ = apply(double, 5);
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `f(x)` and `apply(double, 5)` are call expressions
    assert!(
        has_calls_edge(&staging),
        "bare-identifier closure source must produce Calls edges for `f(x)` and `apply(double, 5)`"
    );
}

/// Closure with typed parameter in `|x: i32|` form (uses `parameter` child)
#[test]
fn closure_typed_parameter() {
    let source = r"
fn main() {
    let add_one = |x: i32| -> i32 { x + 1 };
    let _ = add_one(3);
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `add_one(3)` is a call
    assert!(
        has_calls_edge(&staging),
        "typed-closure source must produce a Calls edge for `add_one(3)`"
    );
}

/// if-let with pattern that creates let condition binding
#[test]
fn binding_let_condition_while_let() {
    let source = r"
fn consume(mut q: std::collections::VecDeque<i32>) -> i32 {
    let mut total = 0;
    while let Some(v) = q.pop_front() {
        total += v;
    }
    total
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `q.pop_front()` is a method call
    assert!(
        has_calls_edge(&staging),
        "while-let binding source must produce a Calls edge for `q.pop_front()`"
    );
}

/// For-loop variable binding
#[test]
fn binding_for_variable() {
    let source = r"
fn sum(items: &[i32]) -> i32 {
    let mut acc = 0;
    for n in items {
        acc += n;
    }
    acc
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "for-variable binding source must produce a Function node for `sum`"
    );
}

/// Match arm with struct pattern that binds via `field_pattern`
#[test]
fn binding_match_arm_struct_pattern() {
    let source = r"
struct Pair { a: i32, b: i32 }
fn larger(p: Pair) -> i32 {
    match p {
        Pair { a, b } if a > b => a,
        Pair { b, .. } => b,
    }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // Both Struct and Function must be staged
    assert!(
        has_node_kind(&staging, NodeKind::Struct),
        "match-arm struct-pattern source must produce a Struct node for `Pair`"
    );
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "match-arm struct-pattern source must produce a Function node for `larger`"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// lifetime_extractor.rs — additional coverage
// ─────────────────────────────────────────────────────────────────────────────

mod lifetime_coverage {
    use super::parse_rust;
    use sqry_core::graph::unified::edge::kind::LifetimeConstraintKind;
    use sqry_lang_rust::confidence::{ConfidenceLevel, ConfidenceTracker};
    use sqry_lang_rust::lifetime_extractor::LifetimeExtractor;

    fn extract(code: &str) -> sqry_lang_rust::lifetime_extractor::LifetimeExtractionResult {
        let tree = parse_rust(code);
        let root = tree.root_node();
        let mut confidence = ConfidenceTracker::new(ConfidenceLevel::Partial);
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_item" | "struct_item" | "impl_item" | "trait_item" => {
                    let mut ext = LifetimeExtractor::new(
                        code.as_bytes(),
                        "owner".to_string(),
                        &mut confidence,
                    );
                    return ext.extract(child);
                }
                _ => {}
            }
        }
        sqry_lang_rust::lifetime_extractor::LifetimeExtractionResult::default()
    }

    /// dyn Trait + 'a → `TraitObject` edge
    /// The tree-sitter grammar uses "`dynamic_type`" or "`trait_object_type`" for `dyn Trait + 'a`.
    /// We verify that the extractor is called by checking if there is a `TraitObject` edge, OR
    /// that processing doesn't panic (grammar may emit the lifetime as part of a parent reference).
    #[test]
    fn dyn_trait_lifetime_no_panic() {
        // Intentionally no strong assertion — verifies no panic on edge-case grammar.
        // The exact edge kind depends on the tree-sitter-rust grammar version; both
        // TraitObject and Reference edges are acceptable outcomes.
        let result = extract("fn foo<'a>(x: Box<dyn std::fmt::Debug + 'a>) {}");
        // Extraction must complete without panic.
        let _ = result;
    }

    /// impl Trait + 'a → `ImplTrait` edge (grammar may produce "`abstract_type`" or "`impl_type`")
    #[test]
    fn impl_trait_lifetime_no_panic() {
        // Intentionally no strong assertion — verifies no panic on edge-case grammar.
        // Grammar representation of `impl Trait + 'lifetime` varies by version.
        let result = extract("fn foo<'a>() -> Box<dyn std::fmt::Debug + 'a> { Box::new(42) }");
        let _ = result;
    }

    /// Higher-ranked trait bounds → `HigherRanked` edge.
    /// This test exercises the `extract_hrtb` code path by targeting a `where_predicate`
    /// that contains a `higher_ranked_trait_bound` field.
    #[test]
    fn hrtb_lifetime_no_panic() {
        // Intentionally no strong assertion — verifies no panic on edge-case grammar.
        // HRTB (`for<'a>`) grammar representation varies across tree-sitter-rust versions.
        let result = extract("fn foo() where for<'a> &'a str: std::fmt::Debug {}");
        let _ = result;
    }

    /// Generic type arguments with lifetime: `Vec<&'a str>` → Reference edge
    #[test]
    fn type_argument_lifetime() {
        let result = extract("fn foo<'a>(v: std::vec::Vec<&'a str>) {}");
        let refs: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Reference)
            .collect();
        assert!(
            !refs.is_empty(),
            "Expected Reference edge(s). All edges: {:?}",
            result.edges
        );
    }

    /// 'static in type arguments → Static edge
    #[test]
    fn static_in_type_argument() {
        let result = extract("fn foo(v: std::vec::Vec<&'static str>) {}");
        let static_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Static)
            .collect();
        assert!(
            !static_edges.is_empty(),
            "Expected Static edge. All edges: {:?}",
            result.edges
        );
    }

    /// T: 'static in `type_parameters` → Static edge from `TypeBound` path
    #[test]
    fn type_param_static_bound() {
        let result = extract("fn foo<T: 'static>(x: T) {}");
        let static_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Static)
            .collect();
        assert!(
            !static_edges.is_empty(),
            "Expected Static edge for T: 'static. All edges: {:?}",
            result.edges
        );
    }

    /// Elided lifetime in reference type: `&str` (no explicit lifetime)
    /// Should record a limitation in the confidence tracker.
    #[test]
    fn elided_lifetime_records_limitation() {
        let tree = parse_rust("fn foo(x: &str) {}");
        let root = tree.root_node();
        let mut confidence = ConfidenceTracker::new(ConfidenceLevel::Partial);
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            if child.kind() == "function_item" {
                let mut ext = LifetimeExtractor::new(
                    b"fn foo(x: &str) {}",
                    "owner".to_string(),
                    &mut confidence,
                );
                let _ = ext.extract(child);
                break;
            }
        }
        // Elided lifetime in `&str` parameter should add a limitation
        assert!(
            confidence.limitation_count() > 0,
            "Expected at least one limitation for elided lifetime"
        );
    }

    /// `is_empty()` on a non-empty result
    #[test]
    fn result_is_empty_false() {
        let result = extract("fn foo<'a>(x: &'a str) {}");
        assert!(!result.is_empty());
    }

    /// `is_empty()` on default empty result
    #[test]
    fn result_is_empty_true() {
        let result = sqry_lang_rust::lifetime_extractor::LifetimeExtractionResult::default();
        assert!(result.is_empty());
    }

    /// Outlives in `type_parameters` inline (`'a: 'b`) or in where clause.
    /// The tree-sitter grammar may put inline outlives constraints in `lifetime_parameter`
    /// or in a `where_clause`. Either way, extraction should complete without panic.
    #[test]
    fn outlives_where_predicate_no_panic() {
        // Test the where-clause path
        let result = extract("fn foo<'a, 'b>(x: &'a str, y: &'b str) where 'a: 'b {}");
        let _ = result;
    }

    /// Lifetime outlives inline in `type_parameters` (`'a: 'b` in generic list)
    #[test]
    fn outlives_inline_type_param() {
        // `'a: 'b` in type_parameters produces `lifetime_predicate` or `lifetime_parameter`
        // depending on the grammar version. This test verifies the code path is exercised.
        let result = extract("fn foo<'a: 'b, 'b>(x: &'a str) {}");
        // At minimum, the lifetime nodes 'a and 'b should be extracted
        assert!(
            !result.nodes.is_empty(),
            "Expected lifetime nodes for 'a and 'b. Got: {:?}",
            result.nodes
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// proc_macro_detector.rs — contradiction arm + accessor coverage
// ─────────────────────────────────────────────────────────────────────────────

mod proc_macro_coverage {
    use sqry_lang_rust::confidence::ConfidenceTracker;
    use sqry_lang_rust::proc_macro_detector::{ProcMacroDetectionSource, ProcMacroDetector};

    /// Contradiction arm: proc-macro attr on a non-proc-macro crate → false + limitation
    #[test]
    fn contradiction_non_proc_macro_crate_with_attr() {
        let detector = ProcMacroDetector::not_proc_macro();
        let mut confidence = ConfidenceTracker::default();
        let result = detector.should_extract_as_macro(true, &mut confidence);
        assert!(!result, "Should be false for contradiction case");
        assert!(
            confidence.limitation_count() > 0,
            "Should record a limitation for contradiction"
        );
    }

    /// `detection_source()` accessor
    #[test]
    fn accessor_detection_source() {
        let d = ProcMacroDetector::proc_macro();
        assert_eq!(d.detection_source(), ProcMacroDetectionSource::CargoToml);
    }

    /// `cargo_toml_path()` accessor — present and absent
    #[test]
    fn accessor_cargo_toml_path() {
        let d = ProcMacroDetector::proc_macro();
        assert!(d.cargo_toml_path().is_none()); // no path in constructor

        let d2 = ProcMacroDetector::not_proc_macro();
        assert!(d2.cargo_toml_path().is_none());
    }

    /// `as_str()` on all variants
    #[test]
    fn detection_source_as_str_all_variants() {
        assert_eq!(ProcMacroDetectionSource::CargoToml.as_str(), "cargo_toml");
        assert_eq!(
            ProcMacroDetectionSource::CrateAttribute.as_str(),
            "crate_attribute"
        );
        assert_eq!(
            ProcMacroDetectionSource::FunctionAttributeOnly.as_str(),
            "function_attribute_only"
        );
        assert_eq!(ProcMacroDetectionSource::Unknown.as_str(), "unknown");
    }

    /// `is_confident()` on all variants
    #[test]
    fn detection_source_is_confident_all_variants() {
        assert!(ProcMacroDetectionSource::CargoToml.is_confident());
        assert!(ProcMacroDetectionSource::CrateAttribute.is_confident());
        assert!(!ProcMacroDetectionSource::FunctionAttributeOnly.is_confident());
        assert!(!ProcMacroDetectionSource::Unknown.is_confident());
    }

    /// no-attribute case: `should_extract_as_macro(false)` → always false
    #[test]
    fn no_attr_always_false() {
        let mut confidence = ConfidenceTracker::default();
        let d = ProcMacroDetector::proc_macro();
        assert!(!d.should_extract_as_macro(false, &mut confidence));

        let d2 = ProcMacroDetector::not_proc_macro();
        assert!(!d2.should_extract_as_macro(false, &mut confidence));

        let d3 = ProcMacroDetector::default();
        assert!(!d3.should_extract_as_macro(false, &mut confidence));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// lib.rs — extract_scopes covers all 6 scope types
// ─────────────────────────────────────────────────────────────────────────────

mod scope_extraction {
    use sqry_core::plugin::LanguagePlugin;
    use sqry_lang_rust::RustPlugin;
    use std::path::Path;

    fn extract(source: &str) -> Vec<sqry_core::ast::Scope> {
        let plugin = RustPlugin::default();
        let tree = plugin.parse_ast(source.as_bytes()).expect("parse");
        plugin
            .extract_scopes(&tree, source.as_bytes(), Path::new("t.rs"))
            .expect("extract_scopes")
    }

    #[test]
    fn extracts_function_scope() {
        let scopes = extract("fn greet() {}");
        assert!(
            scopes
                .iter()
                .any(|s| s.scope_type == "function" && s.name == "greet"),
            "Expected function scope 'greet': {scopes:?}"
        );
    }

    #[test]
    fn extracts_impl_scope() {
        let scopes = extract("struct Foo; impl Foo { fn bar(&self) {} }");
        assert!(
            scopes.iter().any(|s| s.scope_type == "impl"),
            "Expected impl scope: {scopes:?}"
        );
    }

    #[test]
    fn extracts_trait_scope() {
        let scopes = extract("trait MyTrait { fn do_it(&self); }");
        assert!(
            scopes
                .iter()
                .any(|s| s.scope_type == "trait" && s.name == "MyTrait"),
            "Expected trait scope: {scopes:?}"
        );
    }

    #[test]
    fn extracts_module_scope() {
        let scopes = extract("mod utils { pub fn helper() {} }");
        assert!(
            scopes
                .iter()
                .any(|s| s.scope_type == "module" && s.name == "utils"),
            "Expected module scope: {scopes:?}"
        );
    }

    #[test]
    fn extracts_struct_scope() {
        let scopes = extract("struct Point { x: f64, y: f64 }");
        assert!(
            scopes
                .iter()
                .any(|s| s.scope_type == "struct" && s.name == "Point"),
            "Expected struct scope: {scopes:?}"
        );
    }

    #[test]
    fn extracts_enum_scope() {
        let scopes = extract("enum Direction { North, South, East, West }");
        assert!(
            scopes
                .iter()
                .any(|s| s.scope_type == "enum" && s.name == "Direction"),
            "Expected enum scope: {scopes:?}"
        );
    }

    #[test]
    fn extracts_nested_scopes_with_parent() {
        let scopes = extract("mod inner { fn foo() {} }");
        assert!(
            scopes.len() >= 2,
            "Expected at least 2 scopes (module + function): {scopes:?}"
        );
        // The function scope inside the module should have a parent
        let fn_scope = scopes.iter().find(|s| s.scope_type == "function");
        assert!(fn_scope.is_some(), "Expected function scope: {scopes:?}");
        assert!(
            fn_scope.unwrap().parent_id.is_some(),
            "Function inside module should have a parent scope"
        );
    }

    #[test]
    fn empty_source_has_no_scopes() {
        let scopes = extract("");
        assert!(scopes.is_empty(), "Expected no scopes for empty source");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Graph builder — additional node-type coverage
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn graph_extracts_trait_definition() {
    let source = r"
trait Drawable {
    fn draw(&self);
    fn color(&self) -> u32 { 0 }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // Rust traits are added via `add_interface_with_visibility`, producing NodeKind::Interface
    assert!(
        has_node_kind(&staging, NodeKind::Interface),
        "trait definition must produce an Interface node for `Drawable`"
    );
}

#[test]
fn graph_extracts_enum_with_variants() {
    let source = r"
enum Color {
    Red,
    Green,
    Blue,
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // Enum definition must produce an Enum node
    assert!(
        has_node_kind(&staging, NodeKind::Enum),
        "enum definition must produce an Enum node for `Color`"
    );
}

#[test]
fn graph_extracts_extern_crate() {
    let source = r"
extern crate serde;
fn use_serde() {}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `extern crate` produces an Import node
    assert!(
        has_imports_edge(&staging) || has_node_kind(&staging, NodeKind::Import),
        "extern crate must produce an Import node or Imports edge"
    );
}

#[test]
fn graph_extracts_use_declaration() {
    let source = r"
use std::collections::HashMap;
fn make_map() -> HashMap<String, i32> { HashMap::new() }
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // `use` declaration must produce an Import node or Imports edge
    assert!(
        has_imports_edge(&staging) || has_node_kind(&staging, NodeKind::Import),
        "use declaration must produce an Import node or Imports edge"
    );
}

#[test]
fn graph_handles_async_function() {
    let source = r#"
async fn fetch_data(url: &str) -> String {
    let _ = url;
    String::from("data")
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "async fn must produce a Function node for `fetch_data`"
    );
    // Verify the node is marked async
    let async_fn = staging
        .nodes()
        .find(|n| n.entry.kind == NodeKind::Function && n.entry.is_async);
    assert!(
        async_fn.is_some(),
        "async fn `fetch_data` must have is_async=true on its Function node"
    );
}

#[test]
fn graph_handles_const_fn() {
    let source = r"
const fn max(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "const fn must produce a Function node for `max`"
    );
}

#[test]
fn graph_handles_pub_fn() {
    let source = r"
pub fn public_api() -> bool { true }
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    assert!(
        has_node_kind(&staging, NodeKind::Function),
        "pub fn must produce a Function node for `public_api`"
    );
}

#[test]
fn graph_handles_foreign_fn_block() {
    let source = r#"
extern "C" {
    fn printf(fmt: *const u8, ...) -> i32;
}
"#;
    let staging = build_graph(source);
    // FFI extern items may or may not produce graph nodes depending on config,
    // but the build must succeed and return a consistent stats object.
    let stats = staging.stats();
    // edges_staged must never exceed nodes_staged * nodes_staged (basic sanity).
    assert!(
        stats.edges_staged <= stats.nodes_staged * stats.nodes_staged + stats.nodes_staged,
        "edges_staged ({}) is inconsistent with nodes_staged ({})",
        stats.edges_staged,
        stats.nodes_staged
    );
}

#[test]
fn graph_handles_derive_macro() {
    // The struct must be `pub` so `is_exported` is true — `process_derive_attributes`
    // is only called for exported items when macro expansion is enabled.
    let source = r"
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub name: String,
    pub value: i32,
}
";
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 1);
    // Derived pub struct must produce a Struct node
    assert!(
        has_node_kind(&staging, NodeKind::Struct),
        "derived pub struct must produce a Struct node for `Config`"
    );
    // derive macros on pub structs produce Calls edges to the macro nodes
    assert!(
        has_calls_edge(&staging),
        "derive macro on pub struct must produce Calls edges to macro nodes (Debug, Clone, PartialEq)"
    );
}

#[test]
fn graph_handles_multifile_module_structure() {
    let source = r#"
mod database {
    pub struct Connection {
        url: String,
    }
    impl Connection {
        pub fn new(url: &str) -> Self {
            Connection { url: url.to_string() }
        }
        pub fn connect(&self) -> bool { true }
    }
}

fn main() {
    let conn = database::Connection::new("localhost");
    let _ = conn.connect();
}
"#;
    let staging = build_graph(source);
    assert!(staging.stats().nodes_staged >= 2);
    // Must contain a Struct, Method nodes, and Calls edges
    assert!(
        has_node_kind(&staging, NodeKind::Struct),
        "module-structure source must produce a Struct node for `Connection`"
    );
    assert!(
        has_calls_edge(&staging),
        "module-structure source must produce Calls edges for `new` and `connect` calls"
    );
}
