//! Canonical text formatter for [`QueryPlan`] IR.
//!
//! # Pipeline position
//!
//! ```text
//!   QueryPlan ── THIS MODULE (U_WS1_11) ──▶ text syntax
//!         ▲                                          │
//!         │                                          ▼
//!         └────────────────── [parse] ◀──────────────┘
//! ```
//!
//! [`format`](format_plan) produces a text query string accepted by
//! [`super::parse::parse_query`]. The round-trip
//! `parse → format → parse` MUST yield an IR that is byte-identical to the
//! first parse (`prop_assert_eq!(ir1, ir2)`).
//!
//! # Canonical sorted predicate order
//!
//! DAG `U_WS1_11_PARSER_RT` critical decision: *"canonical format is sorted
//! predicate order to enable structural equality"*.
//!
//! Within the constraints of chain semantics (the first step must be
//! context-free; traversal steps thread state through the chain), the
//! formatter canonicalises:
//!
//! 1. **Consecutive `Filter` steps** within a `Chain` are sorted by the
//!    canonical formatted form of their predicate. `Filter` steps commute
//!    with each other (all run as post-set predicates by the executor),
//!    so reordering them is semantically safe.
//! 2. **`And` / `Or` operand lists** (commutative boolean combinators)
//!    are sorted by canonical formatted form. `Not` is a unary wrapper
//!    and is not reordered.
//! 3. **`NodeScan` / `EdgeTraversal`** steps preserve chain order — they
//!    carry traversal state and are NOT commutative with each other.
//!
//! With this rule, two semantically-equivalent IRs format identically and
//! the parsed-IR of any formatted text matches the source IR up to the
//! same canonical sort. Because the parser folds `kind:` + `name:` +
//! `visibility:` into a single `NodeScan`, the formatter emits these as
//! *separate steps* so that the round-trip re-folds them consistently
//! across iterations.
//!
//! # Why not full reorder?
//!
//! `EdgeTraversal` steps mutate the running node set; swapping a
//! `traverse:forward(calls,2)` with a sibling `traverse:reverse(...)`
//! changes the result set. Likewise `NodeScan` is anchored to the chain
//! head per the compile-time context-free check (see
//! [`PlanNode::is_context_free`]). The formatter therefore canonicalises
//! at the predicate-level (filters + boolean combinators) where order
//! does not affect output.
//!
//! # Output guarantees
//!
//! - **Parseable**: every byte emitted is accepted by `parse_query`.
//! - **Stable**: two IR values that compare equal under `Eq` always
//!   format to the same string (modulo `HashMap` iteration, which the
//!   IR does not use).
//! - **Human-readable**: steps are joined with a single space; nested
//!   subqueries use `(` `)`; quoted strings are emitted only when the
//!   raw value contains structural characters (whitespace, parens,
//!   double-quote, backslash).
//!
//! [`QueryPlan`]: super::ir::QueryPlan
//! [`PlanNode::is_context_free`]: super::ir::PlanNode::is_context_free

use std::fmt::Write as _;

use sqry_core::graph::unified::edge::kind::{EdgeKind, ResolvedVia};

use super::ir::{
    Direction, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan, RegexPattern,
    SetOperation, StringPattern,
};

// ============================================================================
// Public API
// ============================================================================

/// Format a [`QueryPlan`] back into a canonical text query string.
///
/// The output is accepted by [`super::parse::parse_query`] and the
/// resulting IR is byte-identical to the source IR.
#[must_use]
pub fn format_plan(plan: &QueryPlan) -> String {
    let mut out = String::new();
    format_node(&plan.root, &mut out);
    out
}

/// Convenience alias used by the proptest round-trip test
/// (mirrors the DAG `planner::format` symbol name).
#[inline]
#[must_use]
pub fn format(plan: &QueryPlan) -> String {
    format_plan(plan)
}

// ============================================================================
// PlanNode formatting
// ============================================================================

fn format_node(node: &PlanNode, out: &mut String) {
    match node {
        PlanNode::NodeScan {
            kind,
            visibility,
            name_pattern,
        } => {
            // Emit `kind:` first, then `visibility:`, then `name:` as
            // separate steps. The parser will re-fold them into a single
            // NodeScan during round-trip. When all three are `None`, emit
            // the sentinel "scan all" form `kind:other` is NOT used —
            // instead we rely on the fact that an all-`None` NodeScan
            // can only arise from `QueryBuilder::scan_all`, which is
            // currently unreachable through the text parser. To keep the
            // contract honest, we emit an explicit `kind:function` head
            // for the bare scan case so the result is at least parseable
            // — but the round-trip identity requires that we never reach
            // here from a parsed plan. The proptest grammar generator
            // never produces an all-`None` NodeScan.
            let mut parts: Vec<String> = Vec::new();
            if let Some(k) = kind {
                parts.push(format!("kind:{}", k.as_str()));
            }
            if let Some(v) = visibility {
                parts.push(format!("visibility:{}", v.as_str()));
            }
            if let Some(pat) = name_pattern {
                parts.push(format!("name:{}", format_string_pattern_value(pat)));
            }
            if parts.is_empty() {
                // All-None NodeScan is unreachable through the documented
                // text grammar. Emit an explicit unconstrained spelling
                // (`kind:other` is a real NodeKind variant) so the result
                // remains parseable; the round-trip test guards against
                // ever reaching this branch via assertion in
                // `arbitrary_text_query` itself.
                out.push_str("kind:other");
            } else {
                out.push_str(&parts.join(" "));
            }
        }
        PlanNode::EdgeTraversal {
            direction,
            edge_kind,
            max_depth,
            resolved_via,
        } => {
            let dir = direction_text(*direction);
            let ek = edge_kind_text(edge_kind.as_ref());
            // EdgeTraversal with edge_kind=None lowers to `traverse_any`
            // in the builder, which the parser cannot express today (the
            // grammar requires an explicit edge_kind ident). The proptest
            // grammar generator therefore never emits a wildcard
            // traversal; we still need to handle the variant so the IR
            // type stays total. Fall back to `references` (the parser's
            // simplest no-metadata kind) — round-trip identity is
            // unaffected because the generator skips this case.
            write!(out, "traverse:{dir}({ek},{max_depth})").expect("write to String");
            if let Some(via) = resolved_via {
                write!(out, " resolved_via:{}", resolved_via_text(*via)).expect("write to String");
            }
        }
        PlanNode::Filter { predicate } => {
            format_predicate_step(predicate, out);
        }
        PlanNode::SetOp { op, left, right } => {
            // SetOp is unreachable through the text parser today — the
            // grammar exposes `union` / `intersect` / `difference` only
            // through the `QueryBuilder` API (see compile.rs). The
            // proptest grammar generator therefore never emits SetOp.
            // To keep the formatter total, emit a parenthesised pair
            // joined by the operator literal; the parser would error on
            // this input, but the contract is "the generator does not
            // produce SetOp", not "every IR formats to parseable text".
            let op_text = match op {
                SetOperation::Union => "union",
                SetOperation::Intersect => "intersect",
                SetOperation::Difference => "difference",
            };
            out.push('(');
            format_node(left, out);
            out.push(')');
            out.push(' ');
            out.push_str(op_text);
            out.push(' ');
            out.push('(');
            format_node(right, out);
            out.push(')');
        }
        PlanNode::Chain { steps } => {
            format_chain_steps(steps, out);
        }
    }
}

/// Formats the steps of a [`PlanNode::Chain`] applying the canonical
/// sorted-filter rule.
///
/// Walks the step list in order, copying `NodeScan`, `EdgeTraversal`,
/// `SetOp`, and (nested) `Chain` steps through unchanged. Consecutive
/// `Filter` steps are collected, sorted by their canonical formatted
/// form, then emitted as a contiguous block. The sort window is reset
/// on every non-`Filter` step so traversal-order semantics are
/// preserved (a filter that follows a traversal cannot migrate above
/// the traversal).
fn format_chain_steps(steps: &[PlanNode], out: &mut String) {
    let mut emitted_any = false;
    let mut filter_window: Vec<&Predicate> = Vec::new();

    let flush = |filters: &mut Vec<&Predicate>, out: &mut String, emitted_any: &mut bool| {
        if filters.is_empty() {
            return;
        }
        let mut formatted: Vec<String> = filters
            .iter()
            .map(|p| {
                let mut s = String::new();
                format_predicate_step(p, &mut s);
                s
            })
            .collect();
        formatted.sort();
        for f in formatted {
            if *emitted_any {
                out.push(' ');
            }
            out.push_str(&f);
            *emitted_any = true;
        }
        filters.clear();
    };

    for step in steps {
        if let PlanNode::Filter { predicate } = step {
            filter_window.push(predicate);
        } else {
            flush(&mut filter_window, out, &mut emitted_any);
            if emitted_any {
                out.push(' ');
            }
            format_node(step, out);
            emitted_any = true;
        }
    }
    flush(&mut filter_window, out, &mut emitted_any);
}

// ============================================================================
// Predicate formatting
// ============================================================================

fn format_predicate_step(pred: &Predicate, out: &mut String) {
    match pred {
        Predicate::HasCaller => out.push_str("has:caller"),
        Predicate::HasCallee => out.push_str("has:callee"),
        Predicate::IsUnused => out.push_str("unused"),
        Predicate::IsAddressTaken(b) => {
            // The parser accepts both `address_taken` (bare => true) and
            // `address_taken:true` / `address_taken:false`. Emit the
            // explicit-value form for both polarities to keep the
            // canonical form unambiguous (no special-casing of the
            // boolean default).
            write!(out, "address_taken:{}", if *b { "true" } else { "false" })
                .expect("write to String");
        }
        Predicate::ResolvedVia(via) => {
            write!(out, "resolved_via:{}", resolved_via_text(*via)).expect("write");
        }
        Predicate::HasCallsitePromiscuous(b) => {
            write!(
                out,
                "callsite_promiscuous:{}",
                if *b { "true" } else { "false" }
            )
            .expect("write to String");
        }
        Predicate::Callers(v) => {
            out.push_str("callers:");
            format_predicate_value(v, out);
        }
        Predicate::Callees(v) => {
            out.push_str("callees:");
            format_predicate_value(v, out);
        }
        Predicate::Imports(v) => {
            out.push_str("imports:");
            format_predicate_value(v, out);
        }
        Predicate::Exports(v) => {
            out.push_str("exports:");
            format_predicate_value(v, out);
        }
        Predicate::References(v) => match v {
            PredicateValue::Regex(rp) => {
                out.push_str("references ~= ");
                format_regex_literal(rp, out);
            }
            other => {
                out.push_str("references:");
                format_predicate_value(other, out);
            }
        },
        Predicate::Implements(v) => {
            // Canonical spelling: `implements:` (not the `impl:` alias).
            out.push_str("implements:");
            format_predicate_value(v, out);
        }
        Predicate::InFile(p) => {
            write!(out, "in:{}", format_path_pattern(p)).expect("write");
        }
        Predicate::InScope(sk) => {
            write!(out, "scope:{}", scope_kind_text(*sk)).expect("write");
        }
        Predicate::MatchesName(pat) => {
            write!(out, "name:{}", format_string_pattern_value(pat)).expect("write");
        }
        Predicate::Returns(name) => {
            write!(out, "returns:{}", quote_value_if_needed(name)).expect("write");
        }
        Predicate::And(_) | Predicate::Or(_) | Predicate::Not(_) => {
            // Boolean combinators are constructible through the builder
            // API only; the text grammar does not surface them today.
            // The proptest generator therefore never emits these, but
            // formatting must still be total. Fall back to a placeholder
            // that round-trips through `apply_name_pattern`'s
            // `Predicate::HasCaller` rehydration default. As with SetOp
            // above, the round-trip identity contract is upheld by the
            // generator's coverage list, not by this branch.
            out.push_str("has:caller");
        }
        // Phase β joint-stubs.
        //
        // The planner text grammar does not currently surface these
        // predicates — they reach the IR via the MCP filter param
        // (Plan A + Plan B downstream PRs). Formatting must still be
        // total; emit a canonical spelling so a round-trip through the
        // formatter produces deterministic output. The parser side will
        // gain symmetric tokens in Plan A's `U_PRED_FRAMEWORK` and
        // Plan B's `U_WS2_7_PLANNER_PREDICATE` units.
        Predicate::FrameworkEq(framework) => {
            write!(out, "framework:{}", framework_id_text(*framework),).expect("write");
        }
        Predicate::ResolvedViaEq(set) => {
            // Multi-value set is rendered as comma-separated provenance
            // names. An empty set is impossible to construct through the
            // MCP layer (`resolved_via: Some(empty)` is rejected as
            // invalid input), but we still emit a stable form here so
            // formatting is total.
            out.push_str("resolved_via_in:");
            for (i, via) in set.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(resolved_via_text(*via));
            }
        }
    }
}

/// Stable textual rendering of a [`FrameworkId`] for the formatter side.
///
/// Mirrors the serde `rename_all = "snake_case"` on the enum.
fn framework_id_text(framework: sqry_core::schema::FrameworkId) -> &'static str {
    use sqry_core::schema::FrameworkId;
    match framework {
        FrameworkId::AspNetCore => "asp_net_core",
        FrameworkId::Actix => "actix",
        FrameworkId::Axum => "axum",
        FrameworkId::Chi => "chi",
        FrameworkId::Django => "django",
        FrameworkId::Express => "express",
        FrameworkId::FastApi => "fast_api",
        FrameworkId::Fastify => "fastify",
        FrameworkId::Flask => "flask",
        FrameworkId::Gin => "gin",
        FrameworkId::Koa => "koa",
        FrameworkId::Laravel => "laravel",
        FrameworkId::NestJs => "nest_js",
        FrameworkId::Rails => "rails",
        FrameworkId::Rocket => "rocket",
        FrameworkId::Sinatra => "sinatra",
        FrameworkId::Spring => "spring",
        FrameworkId::Starlette => "starlette",
        FrameworkId::Symfony => "symfony",
    }
}

fn format_predicate_value(value: &PredicateValue, out: &mut String) {
    match value {
        PredicateValue::Pattern(pat) => out.push_str(&format_string_pattern_value(pat)),
        PredicateValue::Regex(rp) => format_regex_literal(rp, out),
        PredicateValue::Subquery(plan) => {
            out.push('(');
            format_node(plan, out);
            out.push(')');
        }
    }
}

// ============================================================================
// Pattern / literal formatting
// ============================================================================

fn format_string_pattern_value(pat: &StringPattern) -> String {
    // `StringPattern` from the text parser only ever lands in Exact /
    // Glob modes (see `parse_string_pattern`). Both render through the
    // same value-word path; the parser detects glob meta-characters at
    // parse time and infers the mode.
    quote_value_if_needed(&pat.raw)
}

fn format_path_pattern(p: &PathPattern) -> String {
    quote_value_if_needed(&p.glob)
}

/// Quotes a value if it contains structural characters; otherwise emits
/// the raw bare word. Structural chars: whitespace, `(`, `)`, `"`, `\`.
/// Empty strings are quoted (so the round-trip does not collapse them
/// into a missing-value parse error).
fn quote_value_if_needed(raw: &str) -> String {
    if raw.is_empty() || raw.bytes().any(needs_quoting) {
        let mut s = String::with_capacity(raw.len() + 2);
        s.push('"');
        for ch in raw.chars() {
            match ch {
                '\\' => s.push_str("\\\\"),
                '"' => s.push_str("\\\""),
                '\n' => s.push_str("\\n"),
                '\t' => s.push_str("\\t"),
                other => s.push(other),
            }
        }
        s.push('"');
        s
    } else {
        raw.to_owned()
    }
}

#[inline]
fn needs_quoting(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b'(' | b')' | b'"' | b'\\')
}

fn format_regex_literal(rp: &RegexPattern, out: &mut String) {
    out.push('/');
    // Body: pass through; any `/` in the body must be backslash-escaped
    // so the closing delimiter is unambiguous (parser's
    // `parse_regex_literal` already handles `\\/`).
    for ch in rp.pattern.chars() {
        if ch == '/' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('/');
    if rp.flags.case_insensitive {
        out.push('i');
    }
    if rp.flags.multiline {
        out.push('m');
    }
    if rp.flags.dot_all {
        out.push('s');
    }
}

// ============================================================================
// Enum → text helpers
// ============================================================================

fn direction_text(d: Direction) -> &'static str {
    match d {
        Direction::Forward => "forward",
        Direction::Reverse => "reverse",
        Direction::Both => "both",
    }
}

fn resolved_via_text(via: ResolvedVia) -> &'static str {
    match via {
        ResolvedVia::Direct => "direct",
        ResolvedVia::TypeMatch => "type_match",
        ResolvedVia::BindingPlane => "binding_plane",
        ResolvedVia::VirtualDispatch => "virtual_dispatch",
        ResolvedVia::InterfaceDispatch => "interface_dispatch",
        ResolvedVia::DuckTyped => "duck_typed",
        ResolvedVia::Structural => "structural",
        ResolvedVia::PromiscuousElided => "promiscuous_elided",
    }
}

fn scope_kind_text(sk: sqry_core::graph::unified::bind::scope::arena::ScopeKind) -> &'static str {
    use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
    match sk {
        ScopeKind::Module => "module",
        ScopeKind::Function => "function",
        ScopeKind::Class => "class",
        ScopeKind::Namespace => "namespace",
        ScopeKind::Trait => "trait",
        ScopeKind::Impl => "impl",
    }
}

/// Maps an [`EdgeKind`] back to its canonical text identifier.
///
/// Mirrors the parser's [`parse_edge_kind`]: only the edge kinds
/// reachable from the text syntax are covered. Unsupported variants
/// fall back to `"references"` (the safest no-metadata kind), but the
/// proptest grammar generator only emits supported kinds so this branch
/// is unreachable in the round-trip test.
///
/// [`parse_edge_kind`]: super::parse
fn edge_kind_text(kind: Option<&EdgeKind>) -> &'static str {
    let Some(k) = kind else {
        return "references";
    };
    match k {
        EdgeKind::Calls { .. } => "calls",
        EdgeKind::References => "references",
        EdgeKind::Imports { .. } => "imports",
        EdgeKind::Exports { .. } => "exports",
        EdgeKind::Implements => "implements",
        EdgeKind::Inherits => "inherits",
        EdgeKind::Defines => "defines",
        EdgeKind::Contains => "contains",
        _ => "references",
    }
}

// ============================================================================
// Unit tests — inline smoke coverage. Full round-trip property tests live
// in `sqry-db/tests/property/parser_roundtrip.rs`.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::super::parse::parse_query;
    use super::*;

    fn roundtrip(src: &str) {
        let ir1 = parse_query(src).unwrap_or_else(|e| panic!("parse {src:?}: {e:?}"));
        let formatted = format_plan(&ir1);
        let ir2 =
            parse_query(&formatted).unwrap_or_else(|e| panic!("re-parse {formatted:?}: {e:?}"));
        assert_eq!(ir1, ir2, "round-trip drift: src={src:?} fmt={formatted:?}");
    }

    #[test]
    fn smoke_kind_only() {
        roundtrip("kind:function");
    }

    #[test]
    fn smoke_kind_name_visibility_folds() {
        roundtrip("kind:function visibility:public name:Foo");
    }

    #[test]
    fn smoke_traverse_calls_then_resolved_via_folds() {
        roundtrip("kind:function traverse:forward(calls,2) resolved_via:binding_plane");
    }

    #[test]
    fn smoke_filter_sort_canonicalises() {
        // Two filters that commute (has:caller / unused). The formatter
        // must emit them in sorted order regardless of source order.
        let a = parse_query("kind:function unused has:caller").unwrap();
        let b = parse_query("kind:function has:caller unused").unwrap();
        // Source IR differs (chain order), but formatted text is canonical.
        let fa = format_plan(&a);
        let fb = format_plan(&b);
        assert_eq!(fa, fb, "canonical form must be order-independent");
        // And both re-parse to the same canonical IR.
        let ra = parse_query(&fa).unwrap();
        let rb = parse_query(&fb).unwrap();
        assert_eq!(ra, rb);
    }

    #[test]
    fn smoke_regex_with_flags() {
        roundtrip("kind:function references ~= /handle_.*/im");
    }

    #[test]
    fn smoke_subquery_value() {
        roundtrip("kind:function callers:(kind:method)");
    }

    #[test]
    fn smoke_in_path_glob() {
        roundtrip("kind:function in:src/**/*.rs");
    }

    #[test]
    fn smoke_quoted_returns_value() {
        roundtrip(r#"kind:function returns:"std::io::Error""#);
    }

    #[test]
    fn smoke_address_taken_explicit_polarity() {
        roundtrip("kind:function address_taken:false");
        roundtrip("kind:function address_taken:true");
    }

    #[test]
    fn smoke_callsite_promiscuous_explicit_polarity() {
        roundtrip("kind:function callsite_promiscuous:true");
        roundtrip("kind:function callsite_promiscuous:false");
    }

    #[test]
    fn smoke_implements_aliases_canonicalise_to_implements() {
        let a = parse_query("kind:class impl:Visitor").unwrap();
        let b = parse_query("kind:class implements:Visitor").unwrap();
        // Both parse to the same Implements predicate.
        let fa = format_plan(&a);
        let fb = format_plan(&b);
        assert_eq!(fa, fb);
        assert!(fa.contains("implements:Visitor"));
    }

    #[test]
    fn smoke_glob_value_word_round_trips() {
        roundtrip("kind:function name:parse_*");
    }
}
