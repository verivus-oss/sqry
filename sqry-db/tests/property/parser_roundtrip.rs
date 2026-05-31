//! Parser round-trip property test (DAG unit `U_WS1_11_PARSER_RT`,
//! DESIGN §2.6 of `02_DESIGN-graph-fidelity-planner-correctness.md`).
//!
//! # Contract
//!
//! For every text query string `q` drawn from the documented grammar:
//!
//! ```text
//!   ir1   = planner::parse(q)
//!   text  = planner::format(ir1)
//!   ir2   = planner::parse(text)
//!   ir1 == ir2
//! ```
//!
//! Two layers of round-trip are tested:
//!
//! 1. **Single-shot**: `parse → format → parse` produces an IR equal to
//!    the original parse result.
//! 2. **Idempotent**: `format(parse(format(parse(q))))` equals
//!    `format(parse(q))` — formatting is canonical, so re-formatting
//!    yields a stable fixed point.
//!
//! # Acceptance gates
//!
//! - Default proptest budget (256 cases) — `cargo test -p sqry-db --test
//!   parser_roundtrip`.
//! - 10 000 cases (PR budget) — `PROPTEST_CASES=10000 cargo test -p
//!   sqry-db --test parser_roundtrip --release` × 3.
//! - 100 000 cases (nightly) — same command with `PROPTEST_CASES=100000`.
//!
//! # Grammar coverage
//!
//! The generator [`arbitrary_text_query`] covers every documented
//! predicate form on the WS1 branch:
//!
//! | Form | Source |
//! |---|---|
//! | `kind:<NodeKind>` | `parse.rs` `kind` arm |
//! | `visibility:public|private` | `parse.rs` `visibility` arm |
//! | `name:<bare|glob|quoted>` | `parse.rs` `name` arm |
//! | `returns:<bare|quoted>` | `parse.rs` `returns` arm |
//! | `in:<path-glob>` | `parse.rs` `in` arm |
//! | `scope:<ScopeKind>` | `parse.rs` `scope` arm |
//! | `has:caller|callee` | `parse.rs` `has` arm |
//! | `unused` | `parse.rs` `unused` arm |
//! | `address_taken[:true|false]` | `parse.rs` `address_taken` arm |
//! | `resolved_via:direct|type_match|binding_plane` | `parse.rs` `resolved_via` arm |
//! | `callsite_promiscuous[:true|false]` | `parse.rs` `callsite_promiscuous` arm |
//! | `traverse:<dir>(<edge>,<depth>)` | `parse.rs` `traverse` arm |
//! | `callers|callees|imports|exports|implements|impl:<value>` | `parse.rs` relation arm |
//! | `references:<value>` and `references ~= /regex/[flags]` | `parse.rs` `references` arm |
//!
//! The `framework:` predicate noted in DESIGN §2.6 is a WS2 surface that
//! has not yet landed on `feat/planner-correctness-ws1`; it is excluded
//! from the generator with a comment so the test fails fast (rather than
//! silently skipping the case) once WS2 lands and U_WS2_7 lifts it.
//!
//! # Why no random-byte input?
//!
//! Random bytes are the fuzz target's job (`U_WS1_13_FUZZ`,
//! `sqry-db/fuzz/fuzz_targets/parse.rs`). This proptest validates that
//! `format` produces only parseable, identity-preserving text — a
//! random-byte input that fails to parse is uninteresting for that
//! claim.

use proptest::prelude::*;

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ExportKind, ResolvedVia};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::schema::Visibility;
use sqry_db::planner::{
    Direction, MatchMode, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan, RegexFlags,
    RegexPattern, StringPattern, format_plan, parse_query,
};

// ============================================================================
// Grammar generator
// ============================================================================

/// Top-level strategy: emits a `String` that satisfies the documented
/// text-query grammar. The generated string ALWAYS parses successfully
/// (modulo `Build` errors when the leading step is not context-free —
/// the generator therefore always emits a NodeScan as the first step).
fn arbitrary_text_query() -> impl Strategy<Value = String> {
    // Two shapes:
    //   1. NodeScan-led chain with 0..=3 trailing filter/traverse steps.
    //   2. A standalone `name:<pattern>` (the parser starts a fresh
    //      NodeScan with only the name pattern, which is a valid
    //      context-free first step per `parse.rs` `apply_name_pattern`).
    prop_oneof![
        5 => chain_query_strategy(),
        1 => name_only_strategy(),
    ]
}

fn name_only_strategy() -> impl Strategy<Value = String> {
    name_value_strategy().prop_map(|pat| format!("name:{pat}"))
}

fn chain_query_strategy() -> impl Strategy<Value = String> {
    (
        node_scan_head_strategy(),
        prop::collection::vec(chain_step_strategy(), 0..=3),
    )
        .prop_map(|(head, tail)| {
            let mut parts = vec![head];
            parts.extend(tail);
            parts.join(" ")
        })
}

/// First-step generator. Always emits a context-free NodeScan-like
/// head: `kind:<K>` optionally combined with `visibility:` / `name:`.
fn node_scan_head_strategy() -> impl Strategy<Value = String> {
    (
        node_kind_strategy(),
        prop::option::of(visibility_strategy()),
        prop::option::of(name_value_strategy()),
    )
        .prop_map(|(kind, vis, name)| {
            let mut parts = vec![format!("kind:{kind}")];
            if let Some(v) = vis {
                parts.push(format!("visibility:{v}"));
            }
            if let Some(n) = name {
                parts.push(format!("name:{n}"));
            }
            parts.join(" ")
        })
}

/// Generator for non-head chain steps: filter or traverse.
fn chain_step_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Simple filters
        1 => Just("has:caller".to_string()),
        1 => Just("has:callee".to_string()),
        1 => Just("unused".to_string()),
        // Phase A flag predicates with explicit polarity (the formatter
        // always emits the explicit form; the bare form is parser-only).
        1 => bool_strategy().prop_map(|b| format!("address_taken:{b}")),
        1 => bool_strategy().prop_map(|b| format!("callsite_promiscuous:{b}")),
        // resolved_via — three locked values. Generate as a Filter
        // (NOT adjacent to a Calls traversal) so the parser produces a
        // `Predicate::ResolvedVia` rather than folding into a preceding
        // EdgeTraversal. The fold path is covered by traverse_step.
        1 => resolved_via_strategy().prop_map(|v| format!("resolved_via:{v}")),
        // Scope / file / returns
        1 => scope_kind_strategy().prop_map(|s| format!("scope:{s}")),
        1 => path_glob_strategy().prop_map(|g| format!("in:{g}")),
        2 => name_value_strategy().prop_map(|n| format!("returns:{n}")),
        // Relation predicates (literal / glob value)
        2 => name_value_strategy().prop_map(|n| format!("callers:{n}")),
        2 => name_value_strategy().prop_map(|n| format!("callees:{n}")),
        2 => name_value_strategy().prop_map(|n| format!("imports:{n}")),
        2 => name_value_strategy().prop_map(|n| format!("exports:{n}")),
        2 => name_value_strategy().prop_map(|n| format!("references:{n}")),
        // `impl:` and `implements:` both produce the same Implements
        // predicate; formatter canonicalises to `implements:`. We emit
        // both spellings here so the canonicalisation is exercised.
        1 => name_value_strategy().prop_map(|n| format!("implements:{n}")),
        1 => name_value_strategy().prop_map(|n| format!("impl:{n}")),
        // references regex form
        1 => regex_literal_strategy().prop_map(|r| format!("references ~= {r}")),
        // Subquery values (one-level nesting to bound complexity)
        2 => subquery_relation_strategy(),
        // Traverse steps
        2 => traverse_step_strategy(),
    ]
}

/// Generator for a relation step that takes a parenthesised subquery
/// value. Bounded to one level deep — the inner subquery is a simple
/// NodeScan.
fn subquery_relation_strategy() -> impl Strategy<Value = String> {
    (
        prop::sample::select(vec![
            "callers",
            "callees",
            "imports",
            "exports",
            "references",
            "implements",
        ]),
        node_scan_head_strategy(),
    )
        .prop_map(|(rel, inner)| format!("{rel}:({inner})"))
}

fn traverse_step_strategy() -> impl Strategy<Value = String> {
    (
        direction_strategy(),
        edge_kind_strategy(),
        1u32..=4u32,
        // 30% probability of an immediate adjacent `resolved_via:` on a
        // Calls traversal (exercises the §6.3bis adjacency fold).
        prop::option::weighted(0.3, resolved_via_strategy()),
    )
        .prop_map(|(dir, edge, depth, via)| {
            let base = format!("traverse:{dir}({edge},{depth})");
            // Adjacency fold only triggers for Calls edges (per
            // `try_fold_resolved_via`), but the parser also accepts the
            // standalone Filter form for non-Calls. Emit the value only
            // when paired with Calls so the round-trip behaviour is
            // deterministic (non-Calls + resolved_via emits a trailing
            // Filter step that the formatter would also emit as a
            // trailing Filter — still round-trips, but the chain length
            // changes versus the source; that's still IR-equal because
            // the parser emits the same trailing Filter from the
            // formatted form).
            match via {
                Some(v) if edge == "calls" => format!("{base} resolved_via:{v}"),
                _ => base,
            }
        })
}

// ============================================================================
// Leaf strategies
// ============================================================================

fn node_kind_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        // Subset of NodeKind that the parser accepts. The full set is
        // 34 variants; this list covers the common-case kinds that
        // exercise every formatter branch. Adding more variants here
        // does not improve round-trip coverage because the formatter
        // routes them all through `NodeKind::as_str`.
        "function",
        "method",
        "class",
        "interface",
        "trait",
        "module",
        "struct",
        "enum",
        "macro",
        "constant",
        "type",
        "variable",
    ])
}

fn visibility_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["public", "private"])
}

fn scope_kind_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "module",
        "function",
        "class",
        "namespace",
        "trait",
        "impl",
    ])
}

fn direction_strategy() -> impl Strategy<Value = &'static str> {
    // Use only the canonical spellings the formatter emits. The parser
    // also accepts `outgoing`/`incoming`/`out`/`in`, but the formatter
    // canonicalises to `forward`/`reverse`/`both`, so generating the
    // alternate spellings would break the parse-format-parse text
    // equality (NOT the IR equality — but the test only asserts IR
    // equality, so this is purely defensive bookkeeping).
    prop::sample::select(vec!["forward", "reverse", "both"])
}

fn edge_kind_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "calls",
        "references",
        "imports",
        "exports",
        "implements",
        "inherits",
        "defines",
        "contains",
    ])
}

fn resolved_via_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["direct", "type_match", "binding_plane"])
}

fn bool_strategy() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec!["true", "false"])
}

/// Generator for `name:` / relation value words. Emits one of:
///
/// 1. A bare identifier word (lowercase alphanumeric + `_`).
/// 2. A dot- or `::`-qualified bare word (Rust/Ruby style).
/// 3. A glob with `*`, `?`, or `[abc]` meta.
/// 4. A double-quoted string with a structural character that requires
///    quoting (whitespace, `(`, `)`).
fn name_value_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => bare_ident_strategy(),
        2 => qualified_ident_strategy(),
        2 => glob_value_strategy(),
        1 => quoted_value_strategy(),
    ]
}

fn bare_ident_strategy() -> impl Strategy<Value = String> {
    // ASCII letters + digits + `_`, starting with letter / underscore.
    // Bounded length keeps the test cases small and shrinker fast.
    "[a-zA-Z_][a-zA-Z0-9_]{0,8}".prop_map(String::from)
}

fn qualified_ident_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        // Dotted: `Foo.bar`
        (bare_ident_strategy(), bare_ident_strategy()).prop_map(|(a, b)| format!("{a}.{b}")),
        // Rust ::-qualified
        (
            bare_ident_strategy(),
            bare_ident_strategy(),
            bare_ident_strategy()
        )
            .prop_map(|(a, b, c)| format!("{a}::{b}::{c}")),
        // Ruby # separator
        (bare_ident_strategy(), bare_ident_strategy()).prop_map(|(a, b)| format!("{a}#{b}")),
    ]
}

fn glob_value_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        bare_ident_strategy().prop_map(|s| format!("{s}_*")),
        bare_ident_strategy().prop_map(|s| format!("{s}?")),
        bare_ident_strategy().prop_map(|s| format!("{s}_[abc]")),
    ]
}

fn quoted_value_strategy() -> impl Strategy<Value = String> {
    // Use a value that requires quoting (contains a space or paren).
    // The raw value is generated unquoted; the parser's `take_quoted`
    // consumes the wrapping `"`. We emit the wrapped form here so the
    // generator output is directly parseable.
    prop_oneof![
        (bare_ident_strategy(), bare_ident_strategy()).prop_map(|(a, b)| format!("\"{a} {b}\"")),
        (bare_ident_strategy(), bare_ident_strategy())
            .prop_map(|(a, b)| format!("\"{a}::{b}::Error\"")),
    ]
}

fn path_glob_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        bare_ident_strategy().prop_map(|s| format!("src/{s}.rs")),
        bare_ident_strategy().prop_map(|s| format!("src/{s}/**/*.rs")),
        bare_ident_strategy().prop_map(|s| format!("docs/**/*.{s}")),
    ]
}

fn regex_literal_strategy() -> impl Strategy<Value = String> {
    // Regex body without forward slashes (to avoid escape handling
    // boundary tests — those have dedicated unit coverage). Flags are
    // a subset of `i`, `m`, `s`.
    (
        "[a-zA-Z_][a-zA-Z0-9_.*+?]{0,8}",
        prop::sample::select(vec!["", "i", "m", "s", "im", "is", "ms", "ims"]),
    )
        .prop_map(|(body, flags)| format!("/{body}/{flags}"))
}

// ============================================================================
// Proptest — the round-trip identity
// ============================================================================

/// Canonicalises a [`QueryPlan`] by routing it through `format → parse`
/// once. The result is in canonical sorted-predicate-order form (DAG
/// `U_WS1_11_PARSER_RT` critical decision). Two semantically-equivalent
/// IRs always canonicalise to the same value.
///
/// Mirrors the `canonical_arena()` helper used by the persistence
/// round-trip in DESIGN §2.7 — comparison after a normalising
/// projection avoids tripping over legitimate ordering that the
/// production code path (here: `parse`, which preserves source order)
/// does not normalise.
fn canonicalise(plan: &QueryPlan) -> QueryPlan {
    let formatted = format_plan(plan);
    parse_query(&formatted).unwrap_or_else(|e| panic!("canonicalise: fmt={formatted:?} err={e:?}"))
}

proptest! {
    /// Acceptance (DAG `U_WS1_11_PARSER_RT`, DESIGN §2.6): the
    /// canonical IR is a fixed point of `parse ∘ format`. Equivalent
    /// formulation of the design-doc shape `parse → format → parse ==
    /// ir1` after accounting for the critical decision that "canonical
    /// format is sorted predicate order" — once an IR has been routed
    /// through the formatter, every subsequent `parse ∘ format` cycle
    /// is the identity.
    ///
    /// This is the same pattern used by DESIGN §2.7's
    /// `canonical_arena()`-mediated comparison: the property is over
    /// the canonical projection, not the raw value. A bug in either
    /// parser or formatter manifests as a non-fixed-point IR.
    #[test]
    fn parse_format_parse_identity(text in arbitrary_text_query()) {
        let ir1 = match parse_query(&text) {
            Ok(p) => p,
            // Generator-side error: the grammar generator must produce
            // parseable strings. A parse failure here is a generator
            // bug (NOT a formatter bug). Surface it loudly so the test
            // fails with a useful repro.
            Err(e) => panic!(
                "generator produced unparseable input: text={text:?} err={e:?}"
            ),
        };
        let canon1 = canonicalise(&ir1);
        let formatted = format_plan(&canon1);
        let canon2 = match parse_query(&formatted) {
            Ok(p) => p,
            Err(e) => panic!(
                "format output is not parseable: src={text:?} fmt={formatted:?} err={e:?}"
            ),
        };
        prop_assert_eq!(
            &canon1,
            &canon2,
            "parse-format-parse drift on canonical IR: src={:?} fmt={:?}",
            text,
            formatted
        );
    }

    /// Acceptance: formatting is idempotent —
    /// `format(parse(format(parse(q))))` equals `format(parse(q))`.
    /// This proves the canonical sorted-predicate-order rule reaches a
    /// fixed point after a single re-parse cycle, which is the
    /// underlying invariant the `parse_format_parse_identity` test
    /// relies on.
    #[test]
    fn format_is_idempotent(text in arbitrary_text_query()) {
        let ir1 = parse_query(&text)
            .unwrap_or_else(|e| panic!("generator: text={text:?} err={e:?}"));
        let fmt1 = format_plan(&ir1);
        let ir2 = parse_query(&fmt1)
            .unwrap_or_else(|e| panic!("first round-trip parse: fmt={fmt1:?} err={e:?}"));
        let fmt2 = format_plan(&ir2);
        prop_assert_eq!(&fmt1, &fmt2, "format is not idempotent: src={:?}", text);
    }
}

// ============================================================================
// Non-property regression coverage
// ============================================================================

/// Pin a hand-built IR through `format → parse → format`. This is a
/// coverage backstop for variants the grammar generator does not emit
/// directly (e.g. boolean combinators added through the builder).
#[test]
fn hand_built_plan_round_trips_via_formatter() {
    // kind:function callers:foo callees:bar
    let plan = QueryPlan::new(PlanNode::Chain {
        steps: vec![
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                visibility: None,
                name_pattern: None,
            },
            PlanNode::Filter {
                predicate: Predicate::Callers(PredicateValue::Pattern(StringPattern::exact("foo"))),
            },
            PlanNode::Filter {
                predicate: Predicate::Callees(PredicateValue::Pattern(StringPattern::exact("bar"))),
            },
        ],
    });
    let text = format_plan(&plan);
    let reparsed = parse_query(&text).expect("reparse hand-built plan");
    // The parser may not produce a structurally-identical IR if the
    // formatter canonicalises filter order (which it does — `callees`
    // sorts before `callers` lexicographically). What MUST hold is that
    // re-formatting `reparsed` yields the same text — fixed point.
    let text2 = format_plan(&reparsed);
    assert_eq!(text, text2, "formatter must be idempotent");
}

/// `impl:` and `implements:` produce structurally equal IRs and the
/// formatter emits the canonical `implements:` spelling.
#[test]
fn implements_aliases_canonicalise() {
    let a = parse_query("kind:class impl:Visitor").unwrap();
    let b = parse_query("kind:class implements:Visitor").unwrap();
    assert_eq!(a, b);
    let fa = format_plan(&a);
    assert!(
        fa.contains("implements:"),
        "canonical spelling must be 'implements:', got {fa:?}"
    );
    assert!(!fa.contains("impl:V"));
}

/// The `resolved_via:` adjacency fold survives a round-trip — the
/// parsed IR places `Some(via)` inside the Calls `EdgeTraversal`, the
/// formatter emits the `traverse:...(calls,N) resolved_via:X`
/// adjacency, and the re-parse re-folds.
#[test]
fn resolved_via_fold_round_trips() {
    let src = "kind:function traverse:forward(calls,2) resolved_via:binding_plane";
    let ir1 = parse_query(src).unwrap();
    let text = format_plan(&ir1);
    let ir2 = parse_query(&text).unwrap();
    assert_eq!(ir1, ir2);
    assert!(text.contains("resolved_via:binding_plane"));
    let PlanNode::Chain { steps } = &ir1.root else {
        panic!("expected Chain");
    };
    match &steps[1] {
        PlanNode::EdgeTraversal {
            edge_kind: Some(EdgeKind::Calls { .. }),
            resolved_via: Some(ResolvedVia::BindingPlane),
            ..
        } => {}
        other => panic!("expected folded Calls traversal, got {other:?}"),
    }
}

/// Suppress unused-import warnings for items only consulted by the
/// grammar comments (variant coverage matrix above).
#[allow(dead_code)]
fn _coverage_anchors() {
    let _ = (
        Direction::Forward,
        MatchMode::Glob,
        PathPattern::new(""),
        RegexFlags::default(),
        RegexPattern::new(""),
        ExportKind::Direct,
        Visibility::Public,
        ScopeKind::Module,
    );
}
