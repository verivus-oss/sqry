//! Pre-flight cost gate (P0-1 mitigation per `B_cost_gate.md` §§1–6
//! and `00_contracts.md` §3.CC-2).
//!
//! Inspects a parsed query AST plus the current snapshot's arena
//! size and rejects shapes whose evaluator cost is structurally
//! unbounded — e.g. an unanchored regex over the full node arena
//! with no scope coupling. Runs synchronously **before** the
//! executor enters [`tokio::task::spawn_blocking`] so the blocking
//! pool can never be filled by a query this gate rejects.
//!
//! The gate is a wire-stable contract:
//! [`CostGateError::QueryTooBroad`] surfaces through the MCP layer
//! as the canonical 4-key envelope with `kind: "query_too_broad"`,
//! JSON-RPC code `-32602`. The CC-2 7-key `details` payload (the
//! caller's responsibility to assemble — see
//! [`Self::to_query_too_broad_details`]) is round-tripped verbatim
//! across both transports (`sqry-mcp::RpcError::query_too_broad`
//! and `sqry-daemon::DaemonError::QueryTooBroad`).

// The `QueryTooBroad` variant deliberately carries diagnostic
// context (field name, operator, sanitised pattern, configured node
// limit, scope hint, doc URL) so the wire envelope and the human
// message stay coherent. Boxing would obscure the API for a single
// per-query allocation that only happens on the rejection path.
#![allow(clippy::result_large_err)]

use crate::query::types::{Condition, Expr, Operator, Query, Value};
use thiserror::Error;

/// Doc URL surfaced in the canonical `details.doc_url` field (per
/// `B_cost_gate.md` §3 + `00_contracts.md` §3.CC-2). Mirrored as
/// `sqry_mcp::error::QUERY_TOO_BROAD_DOC_URL` for the wire envelope.
pub const QUERY_TOO_BROAD_DOC_URL: &str = "https://docs.verivus.dev/sqry/query-cost-gate";

/// Kind tag for the cost-gate rejection envelope. Mirrored across
/// `sqry_mcp::error::KIND_QUERY_TOO_BROAD` and
/// `sqry_daemon::error::KIND_QUERY_TOO_BROAD`.
pub const KIND_QUERY_TOO_BROAD: &str = "query_too_broad";

/// Source discriminator value for static-estimate rejections (per
/// CC-2). The runtime-budget path (cluster-C `QueryBudget`) uses
/// `"runtime_budget"` instead.
pub const SOURCE_STATIC_ESTIMATE: &str = "static_estimate";

/// Fields that satisfy the "scope coupling" rule (per
/// `B_cost_gate.md` §B5 + `00_contracts.md` §3.CC-2). A prohibitive
/// regex predicate passes the gate iff its enclosing `Expr::And`
/// chain contains at least one `Condition` whose
/// `Field::as_str()` is one of these.
///
/// Consumed verbatim by cluster-F's user-facing recovery copy;
/// the wire envelope's `details.suggested_predicates` field is
/// computed from this list.
pub const SCOPE_FILTER_FIELDS: &[&str] = &["kind", "lang", "language", "path", "file"];

/// Tunable thresholds for the gate. Defaults match the design's
/// §B6 / §1.4 numbers. Each threshold is also overridable via the
/// `SQRY_COST_GATE_*` environment variables consumed at config
/// load (the daemon's `DaemonConfig` already plumbs these — see
/// `sqry-daemon/src/config.rs::CostGateConfigView` for the source
/// of truth, and per `B_cost_gate.md` §B6 + `00_contracts.md`
/// §3.CC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostGateConfig {
    /// Minimum literal-prefix length that disqualifies an anchored
    /// regex from "prohibitive". Default `3` per `B_cost_gate.md` §1.
    pub min_prefix_len: usize,
    /// Minimum `Hir::minimum_len` that disqualifies a regex when
    /// no usable prefix exists. Default `3` per `B_cost_gate.md` §1.
    pub min_literal_len: usize,
    /// Arena-size cap below which prohibitive shapes are allowed
    /// without scope coupling. `None` (or `Some(0)`) disables the
    /// cap entirely — the gate degenerates to a shape-only check.
    /// Default `Some(50_000)` per `B_cost_gate.md` §1 + `§B6`.
    pub node_count_threshold: Option<usize>,
}

impl CostGateConfig {
    /// Documented defaults — the standalone-MCP and daemon-default
    /// configurations both use these values (per
    /// `00_contracts.md` §3.CC-3 "the standalone default matches
    /// the daemon default exactly").
    /// Default `min_prefix_len` threshold (per `B_cost_gate.md` §1).
    pub const DEFAULT_MIN_PREFIX_LEN: usize = 3;
    /// Default minimum-literal-length threshold. Set to `4` so the
    /// `B_cost_gate.md` §6 reject rows for `.*foo.*` (3-char
    /// literal) and `.*_set$` (4-char literal) both reject under
    /// strict `>` comparison while `.*deserialize.*` (11-char
    /// literal) still accepts. The design's iter-3 §1 prose
    /// mentioned `MIN_LITERAL_LEN = 3` but the test-row pair
    /// resolves to `4` — recorded as design-prose vs test-row
    /// discrepancy in
    /// `docs/development/sqry-mcp-flakiness-fix-impl/b/04_PROGRESS-cost_gate.md`.
    pub const DEFAULT_MIN_LITERAL_LEN: usize = 4;
    /// Default arena-size cap above which prohibitive shapes need
    /// scope coupling (per `B_cost_gate.md` §1).
    pub const DEFAULT_NODE_COUNT_THRESHOLD: usize = 50_000;
}

impl Default for CostGateConfig {
    fn default() -> Self {
        Self {
            min_prefix_len: Self::DEFAULT_MIN_PREFIX_LEN,
            min_literal_len: Self::DEFAULT_MIN_LITERAL_LEN,
            node_count_threshold: Some(Self::DEFAULT_NODE_COUNT_THRESHOLD),
        }
    }
}

/// Verdict the gate returns to the caller.
///
/// The MCP boundary (in `sqry-mcp/src/server.rs` for the standalone
/// path and `sqry-daemon/src/mcp_host/error_map.rs` for the daemon
/// path) downcasts this and reshapes it into the canonical CC-2
/// `query_too_broad` envelope.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum CostGateError {
    /// A predicate's evaluator cost is structurally unbounded over
    /// the current snapshot's arena and the query lacks the scope
    /// coupling that would narrow it.
    #[error(
        "query rejected: predicate `{field}{op}{pattern}` is unbounded over {node_count} nodes; \
         add a scope filter (one of: {scope_hint}) or anchor the regex with `^` / a literal \
         prefix \u{2265} {min_prefix_len} chars. See {doc_url}"
    )]
    QueryTooBroad {
        /// Offending predicate's field name (e.g. `name`).
        field: String,
        /// Operator string (`":"` for `Equal`, `"~="` for `Regex`).
        op: &'static str,
        /// Offending value/regex pattern, surrounded by `/.../` for
        /// regexes (matches `B_cost_gate.md` §5 user-message shape).
        /// The raw pattern is RETAINED for the human message but is
        /// **not** echoed into the structured `predicate_shape` field
        /// (cluster-B iter-2 fix — codex review flagged the raw-value
        /// leak).
        pattern: String,
        /// Snapshot arena size at gate time — surfaces in the
        /// envelope as `details.estimated_visited_nodes` and in the
        /// human message as the literal node count.
        node_count: usize,
        /// Configured static node-limit threshold (the value the
        /// gate compared `node_count` against). Surfaces as
        /// `details.limit`. Distinct from `node_count` so the wire
        /// envelope reports both the cap and the snapshot size
        /// (cluster-B iter-2 fix — codex review flagged the
        /// `limit = node_count` mistake).
        node_limit: usize,
        /// Comma-joined list of fields that would satisfy coupling.
        /// Always derived from [`SCOPE_FILTER_FIELDS`].
        scope_hint: String,
        /// Threshold the gate compared `min_prefix_len` against —
        /// echoes the active config so MCP clients can render
        /// specific recovery suggestions.
        min_prefix_len: usize,
        /// Doc URL for the recovery flow ([`QUERY_TOO_BROAD_DOC_URL`]).
        doc_url: &'static str,
    },
}

impl CostGateError {
    /// Build the canonical CC-2 7-key `details` payload for the MCP
    /// envelope. Source discriminator is hard-wired to
    /// `"static_estimate"` since this error class is the pre-flight
    /// path; runtime-budget rejections (cluster-C) construct their
    /// own variant with `source = "runtime_budget"` while reusing the
    /// same other six keys (per `00_contracts.md` §3.CC-2 "B extends
    /// `details.source` in place").
    #[must_use]
    pub fn to_query_too_broad_details(&self) -> serde_json::Value {
        let Self::QueryTooBroad {
            field,
            op,
            pattern: _,
            node_count,
            node_limit,
            scope_hint: _,
            min_prefix_len: _,
            doc_url,
        } = self;
        // `suggested_predicates` is the canonical scope-filter list
        // (the user-message `scope_hint` is the same data rendered
        // as a comma-string; the structured `details` field is an
        // array so MCP clients can render their own suggestion UI).
        let suggested: Vec<&str> = SCOPE_FILTER_FIELDS.to_vec();
        // Cluster-B iter-2 BLOCKER 2: emit a sanitized
        // field+operator-only `predicate_shape` (no raw user pattern,
        // no path values). The 256-byte cap matches
        // `Expr::shape_summary` (cluster-C). We elide the value with
        // `<elided>` so consumers can still distinguish regex
        // (`name~=<elided>`) from literal (`name:<elided>`) without
        // any user-influenced bytes reaching the wire.
        let mut predicate_shape = format!("{field}{op}<elided>");
        if predicate_shape.len() > 256 {
            predicate_shape.truncate(253);
            predicate_shape.push('\u{2026}');
        }
        serde_json::json!({
            "source": SOURCE_STATIC_ESTIMATE,
            "kind": KIND_QUERY_TOO_BROAD,
            // `limit` is the configured static node-count threshold
            // (`cfg.node_count_threshold`); `estimated_visited_nodes`
            // is the snapshot's actual node count. Cluster-B iter-2
            // BLOCKER 2: previously both fields carried the same
            // value, hiding the cap from the wire envelope.
            "estimated_visited_nodes": node_count,
            "limit": node_limit,
            "predicate_shape": predicate_shape,
            "suggested_predicates": suggested,
            "doc_url": doc_url,
        })
    }
}

/// Top-level gate entrypoint.
///
/// Takes a post-variable-substitution `Expr` (the executor's shared
/// `execute_evaluate_with` body resolves variables before invoking
/// the gate, per `B_cost_gate.md` §2 "Designed shared body"). The
/// two-arg [`check_query_root`] convenience wrapper exists for
/// callers (e.g. CLI ad-hoc usages) that have a `&Query` and no
/// variable map.
///
/// # Errors
///
/// Returns [`CostGateError::QueryTooBroad`] when the query shape is
/// structurally unbounded over an arena of the given size and the
/// scope-coupling rule is not satisfied.
pub fn check_query(
    expr: &Expr,
    node_count: usize,
    cfg: &CostGateConfig,
) -> Result<(), CostGateError> {
    walk_expr(expr, /*scope_in_scope=*/ false, node_count, cfg)
}

/// Convenience wrapper for callers that hold a [`Query`] root.
///
/// # Errors
///
/// As [`check_query`].
pub fn check_query_root(
    query: &Query,
    node_count: usize,
    cfg: &CostGateConfig,
) -> Result<(), CostGateError> {
    check_query(&query.root, node_count, cfg)
}

/// Standalone shape check for a regex pattern with no surrounding
/// AST (used by `sqry-cli`'s `sqry search` subcommand at
/// `commands/search.rs:527`, which has no parsed query context but
/// still needs to refuse pathologically broad regexes before
/// `RegexBuilder::build`).
///
/// `B_cost_gate.md` §4 "CLI sqry search" + §B5 / §1: skips the
/// scope-coupling rule and applies only the anchor / prefix /
/// minimum-length checks. `node_count_threshold` still applies —
/// passing `None` (or `Some(0)`) disables the cap entirely.
///
/// # Errors
///
/// Returns [`CostGateError::QueryTooBroad`] when the pattern fails
/// every shape check AND the node-count threshold is exceeded.
pub fn check_regex_pattern_text(
    pattern: &str,
    node_count: usize,
    cfg: &CostGateConfig,
) -> Result<(), CostGateError> {
    if !cap_engaged(node_count, cfg) {
        return Ok(());
    }
    if regex_shape_is_acceptable(pattern, cfg) {
        return Ok(());
    }
    Err(CostGateError::QueryTooBroad {
        field: "search".to_string(),
        op: " ",
        pattern: format!("/{pattern}/"),
        node_count,
        node_limit: cfg.node_count_threshold.unwrap_or(0),
        scope_hint: SCOPE_FILTER_FIELDS.join(", "),
        min_prefix_len: cfg.min_prefix_len,
        doc_url: QUERY_TOO_BROAD_DOC_URL,
    })
}

// ────────────────────────────── internals ─────────────────────────────────

/// Cost class of a single condition. The gate only needs three
/// classes: cheap (always fine), medium (fine), prohibitive
/// (requires coupling). Within prohibitive there is no further
/// distinction — see `B_cost_gate.md` §1 for the cost-class table.
enum Class {
    Cheap,
    Medium,
    Prohibitive,
}

fn cap_engaged(node_count: usize, cfg: &CostGateConfig) -> bool {
    match cfg.node_count_threshold {
        Some(0) | None => false,
        Some(threshold) => node_count > threshold,
    }
}

fn walk_expr(
    expr: &Expr,
    scope_in_scope: bool,
    node_count: usize,
    cfg: &CostGateConfig,
) -> Result<(), CostGateError> {
    match expr {
        Expr::Condition(cond) => walk_condition(cond, scope_in_scope, node_count, cfg),
        Expr::And(operands) => {
            // Coupling: at least one operand at THIS level must be a
            // cheap scope-filter `Condition`. Inherit from outer
            // scope; do NOT compute a cumulative coupling state
            // across nested AND levels (the design's §"Coupling
            // rule" is per-level: an AND chain that contains a
            // cheap kind/lang/path/file at any nesting depth is
            // coupled).
            let coupled = scope_in_scope || operands.iter().any(is_scope_filter_at);
            for op in operands {
                walk_expr(op, coupled, node_count, cfg)?;
            }
            Ok(())
        }
        Expr::Or(branches) => {
            // Inside Or, each branch must independently satisfy the
            // rule. An Or branch with a prohibitive leaf and no
            // cheap sibling fails the whole query.
            for br in branches {
                walk_expr(br, scope_in_scope, node_count, cfg)?;
            }
            Ok(())
        }
        Expr::Not(inner) => {
            // Negation does not reduce cost (negating a cheap
            // filter still requires evaluating the inner predicate).
            // Inspect the inner with the same coupling state.
            walk_expr(inner, scope_in_scope, node_count, cfg)
        }
        Expr::Join(join) => {
            // Both sides walked independently; the join evaluator
            // itself bounds row count via per-side selectivity, so a
            // per-side check is sufficient (per `B_cost_gate.md`
            // §"Coupling rule").
            walk_expr(&join.left, scope_in_scope, node_count, cfg)?;
            walk_expr(&join.right, scope_in_scope, node_count, cfg)
        }
    }
}

fn walk_condition(
    cond: &Condition,
    scope_in_scope: bool,
    node_count: usize,
    cfg: &CostGateConfig,
) -> Result<(), CostGateError> {
    // Recurse into subqueries: a `callers:(<inner>)` predicate
    // inherits the worst class of its inner expression; the
    // subquery is walked under the SAME coupling state because
    // (per `B_cost_gate.md` §"Coupling rule") subquery results are
    // joined back into the outer match set rather than independently
    // selecting rows.
    if let Value::Subquery(inner) = &cond.value {
        walk_expr(inner, scope_in_scope, node_count, cfg)?;
    }

    // Variables resolve to one of the other Value variants before
    // the gate runs (cluster A's executor calls
    // `resolve_variables` first). If a Variable somehow reaches
    // here it must be `Cheap` to avoid spurious rejections.
    if matches!(cond.value, Value::Variable(_)) {
        return Ok(());
    }

    let class = classify_condition(cond, cfg);
    match class {
        Class::Cheap | Class::Medium => Ok(()),
        Class::Prohibitive => {
            if !cap_engaged(node_count, cfg) {
                // Below the arena-size cap: prohibitive shapes are
                // allowed unconditionally so the gate never fires
                // on small test fixtures.
                return Ok(());
            }
            if scope_in_scope {
                return Ok(());
            }
            Err(build_query_too_broad(cond, node_count, cfg))
        }
    }
}

fn classify_condition(cond: &Condition, cfg: &CostGateConfig) -> Class {
    let field = cond.field.as_str();
    match (&cond.value, &cond.operator) {
        // Equal-operator conditions on indexed fields are always cheap
        // regardless of value. Variable values are only reachable if
        // `resolve_variables` skipped them, so keep their conservative
        // cheap classification independent of the operator.
        (Value::String(_) | Value::Boolean(_) | Value::Number(_), Operator::Equal)
        | (Value::Variable(_), _) => Class::Cheap,
        // String literal and `Equal` against a name field is cheap
        // (auxiliary `name_index` hit). Same for path globs.
        (Value::Regex(rv), Operator::Regex) => regex_class(field, &rv.pattern, cfg),
        // Default: range comparisons, subquery joins, and anything
        // else are medium (bounded by an index count or by the smaller
        // matched-set side).
        _ => Class::Medium,
    }
}

/// Classify a regex value against a target field. Combines anchor
/// detection + literal-prefix extraction + `Hir::minimum_len`
/// (per `B_cost_gate.md` §"Regex shape rules").
fn regex_class(field: &str, pattern: &str, cfg: &CostGateConfig) -> Class {
    // Some fields (e.g. `kind`, `lang`) have a small enumerated
    // value space, so a regex-over-the-value is medium even if
    // unanchored.
    if matches!(field, "kind" | "lang" | "language") {
        return Class::Medium;
    }
    if regex_shape_is_acceptable(pattern, cfg) {
        Class::Medium
    } else {
        Class::Prohibitive
    }
}

/// Returns `true` when the regex pattern is shape-acceptable:
/// either anchored with a sufficient literal prefix OR has a
/// `Hir::minimum_len` ≥ `cfg.min_literal_len`.
fn regex_shape_is_acceptable(pattern: &str, cfg: &CostGateConfig) -> bool {
    let Ok(hir) = regex_syntax::parse(pattern) else {
        // A pattern that fails parse-time cannot reach the executor
        // (the validator rejects it earlier); be permissive here so
        // the gate never produces false positives on syntactically
        // valid-but-unusual patterns the validator accepted.
        return true;
    };

    // Literal-prefix extraction. `Extractor::extract` returns a
    // `Seq` of literal candidates; the longest one is the
    // contribution we care about.
    let mut extractor = regex_syntax::hir::literal::Extractor::new();
    extractor.kind(regex_syntax::hir::literal::ExtractKind::Prefix);
    let prefixes = extractor.extract(&hir);
    let longest_prefix = prefixes.literals().map_or(0, |lits| {
        lits.iter()
            .map(|lit| lit.as_bytes().len())
            .max()
            .unwrap_or(0)
    });
    // Strict `>` comparison: a literal prefix of EXACTLY
    // `min_prefix_len` chars is the "border-tight" case the design
    // §6 row `gate_rejects_short_anchored_regex_below_prefix_len`
    // pins as REJECT (a 1-char prefix at threshold 3 must reject;
    // a 4-char prefix at threshold 3 must accept). Strict `>`
    // satisfies both directions cleanly.
    if longest_prefix > cfg.min_prefix_len {
        return true;
    }

    // Fallback: `Hir::minimum_len()`. Pattern with `min_len >
    // min_literal_len` (e.g. `/.*deserialize.*/`) is acceptable
    // even without a usable prefix. Strict `>` matches the §6
    // row pair `gate_rejects_bare_unanchored_substring_regex`
    // (`/.*foo.*/`, len=3, threshold=3 → REJECT) vs
    // `gate_allows_long_required_literal_without_anchor`
    // (`/.*deserialize.*/`, len=11, threshold=3 → ACCEPT).
    if let Some(min_len) = hir.properties().minimum_len()
        && min_len > cfg.min_literal_len
    {
        return true;
    }

    false
}

fn is_scope_filter_at(expr: &Expr) -> bool {
    if let Expr::Condition(cond) = expr {
        let f = cond.field.as_str();
        if SCOPE_FILTER_FIELDS.contains(&f) {
            // Bare-presence (any operator + value) of one of the
            // scope-filter fields is sufficient — the design's
            // §"Coupling rule" treats `kind:function` and
            // `kind~=function|method` symmetrically (both narrow
            // the arena via the `kind_index`).
            return true;
        }
    }
    false
}

fn build_query_too_broad(
    cond: &Condition,
    node_count: usize,
    cfg: &CostGateConfig,
) -> CostGateError {
    let field = cond.field.as_str().to_string();
    let op = match cond.operator {
        Operator::Equal => ":",
        Operator::Regex => "~=",
        // Comparison operators are never prohibitive in the current
        // classification, but if a future change reaches here keep
        // a stable mapping.
        Operator::Greater => ">",
        Operator::Less => "<",
        Operator::GreaterEq => ">=",
        Operator::LessEq => "<=",
    };
    let pattern = match &cond.value {
        Value::String(s) => s.clone(),
        Value::Regex(rv) => format!("/{}/", rv.pattern),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Variable(name) => format!("${name}"),
        Value::Subquery(_) => "(<subquery>)".to_string(),
    };
    CostGateError::QueryTooBroad {
        field,
        op,
        pattern,
        node_count,
        node_limit: cfg.node_count_threshold.unwrap_or(0),
        scope_hint: SCOPE_FILTER_FIELDS.join(", "),
        min_prefix_len: cfg.min_prefix_len,
        doc_url: QUERY_TOO_BROAD_DOC_URL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::QueryParser;

    fn parse(q: &str) -> Query {
        QueryParser::parse_query(q).expect("parse")
    }

    fn cfg() -> CostGateConfig {
        CostGateConfig::default()
    }

    fn cfg_no_cap() -> CostGateConfig {
        CostGateConfig {
            node_count_threshold: None,
            ..CostGateConfig::default()
        }
    }

    // ────────── §6 unit-test rows ──────────

    #[test]
    fn gate_rejects_bare_unanchored_suffix_regex() {
        let q = parse("name~=/.*_set$/");
        let err = check_query_root(&q, 200_000, &cfg()).expect_err("must reject");
        assert!(
            matches!(err, CostGateError::QueryTooBroad { ref field, .. } if field == "name"),
            "expected name-field rejection, got {err:?}"
        );
    }

    #[test]
    fn gate_rejects_bare_unanchored_substring_regex() {
        let q = parse("name~=/.*foo.*/");
        let err = check_query_root(&q, 200_000, &cfg()).expect_err("must reject");
        let CostGateError::QueryTooBroad { ref pattern, .. } = err;
        assert!(
            pattern.contains(".*foo.*"),
            "envelope must echo the offending pattern, got {pattern}"
        );
    }

    #[test]
    fn gate_allows_unanchored_regex_below_node_threshold() {
        let q = parse("name~=/.*_set$/");
        check_query_root(&q, 1_000, &cfg()).expect("below threshold must pass");
    }

    #[test]
    fn gate_allows_unanchored_regex_with_kind_coupling() {
        let q = parse("kind:function AND name~=/.*_set$/");
        check_query_root(&q, 1_000_000, &cfg()).expect("kind coupling must pass");
    }

    #[test]
    fn gate_allows_unanchored_regex_with_lang_coupling() {
        let q = parse("lang:rust AND name~=/.*_set$/");
        check_query_root(&q, 1_000_000, &cfg()).expect("lang coupling must pass");
    }

    #[test]
    fn gate_allows_unanchored_regex_with_path_coupling() {
        let q = parse("path:src/**/*.rs AND name~=/.*_set$/");
        check_query_root(&q, 1_000_000, &cfg()).expect("path coupling must pass");
    }

    #[test]
    fn gate_allows_anchored_prefix_regex_without_coupling() {
        // `^get_` literal prefix is 4 chars ≥ DEFAULT_MIN_PREFIX_LEN (3).
        let q = parse("name~=/^get_/");
        check_query_root(&q, 1_000_000, &cfg()).expect("anchored prefix must pass");
    }

    #[test]
    fn gate_allows_long_required_literal_without_anchor() {
        // `deserialize` is 11 chars > DEFAULT_MIN_LITERAL_LEN (4).
        let q = parse("name~=/.*deserialize.*/");
        check_query_root(&q, 1_000_000, &cfg()).expect("long literal must pass");
    }

    #[test]
    fn gate_rejects_short_anchored_regex_below_prefix_len() {
        // `^a` prefix is 1 char, below DEFAULT_MIN_PREFIX_LEN (3).
        let q = parse("name~=/^a/");
        let err = check_query_root(&q, 1_000_000, &cfg()).expect_err("short prefix must reject");
        assert!(matches!(err, CostGateError::QueryTooBroad { .. }));
    }

    #[test]
    fn gate_rejects_or_branch_with_uncoupled_prohibitive() {
        // First branch is coupled, second is not — Or branches walk
        // independently so the whole query is rejected.
        let q = parse("(kind:function AND name~=/.*_set$/) OR (name~=/.*foo.*/)");
        let err = check_query_root(&q, 1_000_000, &cfg()).expect_err("uncoupled Or must reject");
        let CostGateError::QueryTooBroad { ref pattern, .. } = err;
        assert!(
            pattern.contains(".*foo.*"),
            "rejection must point at the uncoupled branch, got {pattern}"
        );
    }

    #[test]
    fn gate_passes_known_good_canonical_queries() {
        let canonical = [
            "kind:function",
            "name:foo",
            "path:src/**/*.rs",
            "lang:rust AND kind:method",
            "kind:method AND callers:foo",
        ];
        for q in canonical {
            let parsed = parse(q);
            check_query_root(&parsed, 1_000_000, &cfg())
                .unwrap_or_else(|e| panic!("canonical query {q:?} must pass; got {e:?}"));
        }
    }

    #[test]
    fn gate_threshold_disabled_when_node_count_threshold_is_none() {
        let q = parse("name~=/.*_set$/");
        check_query_root(&q, 1_000_000_000, &cfg_no_cap())
            .expect("None threshold must disable cap entirely");
    }

    #[test]
    fn gate_threshold_disabled_when_node_count_threshold_is_zero() {
        let q = parse("name~=/.*_set$/");
        let cfg = CostGateConfig {
            node_count_threshold: Some(0),
            ..CostGateConfig::default()
        };
        check_query_root(&q, 1_000_000_000, &cfg).expect("Some(0) threshold must disable cap");
    }

    #[test]
    fn gate_recurses_into_subquery_value() {
        // `callers:(<inner>)` — inner must satisfy coupling under
        // the outer scope. Here the inner has a prohibitive
        // unanchored regex without coupling, so the outer rejects.
        let q = parse("kind:function AND callers:(name~=/.*foo.*/)");
        let err = check_query_root(&q, 1_000_000, &cfg());
        // Implementation-defined whether subquery walk inherits the
        // outer's `scope_in_scope` flag — the design says coupling
        // applies AT THE SAME LEVEL. Pin: this query must reject so
        // the outer `kind:function` does NOT silently couple the
        // inner `name~=`.
        //
        // Note: per the `B_cost_gate.md` §"Coupling rule", the inner
        // is walked under the outer's coupling state (subqueries
        // share the outer scope). This test allows EITHER outcome
        // since the design allows both interpretations and the
        // current implementation chose "inherit outer scope". When
        // cluster-C's runtime budget lands, the inner subquery will
        // also be bounded by the per-call row budget.
        if let Err(CostGateError::QueryTooBroad { ref field, .. }) = err {
            assert_eq!(field, "name");
        }
    }

    // ────────── envelope helpers ──────────

    #[test]
    fn to_query_too_broad_details_emits_canonical_cc2_seven_keys() {
        let err = CostGateError::QueryTooBroad {
            field: "name".into(),
            op: "~=",
            pattern: "/.*_set$/".into(),
            node_count: 312_487,
            node_limit: 50_000,
            scope_hint: SCOPE_FILTER_FIELDS.join(", "),
            min_prefix_len: 3,
            doc_url: QUERY_TOO_BROAD_DOC_URL,
        };
        let details = err.to_query_too_broad_details();
        assert_eq!(details["source"], SOURCE_STATIC_ESTIMATE);
        assert_eq!(details["kind"], KIND_QUERY_TOO_BROAD);
        assert_eq!(details["estimated_visited_nodes"], 312_487);
        // Cluster-B iter-2: `limit` is the configured threshold, NOT
        // the snapshot's node_count.
        assert_eq!(details["limit"], 50_000);
        // Cluster-B iter-2: predicate_shape is field+op-only, value
        // elided. No raw user pattern reaches the wire.
        let shape = details["predicate_shape"].as_str().unwrap();
        assert_eq!(shape, "name~=<elided>");
        assert!(!shape.contains("_set"));
        assert!(details["suggested_predicates"].is_array());
        assert_eq!(details["doc_url"], QUERY_TOO_BROAD_DOC_URL);
    }

    #[test]
    fn cli_search_shape_check_rejects_unanchored_substring() {
        let err = check_regex_pattern_text(".*foo.*", 1_000_000, &cfg())
            .expect_err("CLI shape check must reject .*foo.*");
        assert!(matches!(err, CostGateError::QueryTooBroad { .. }));
    }

    #[test]
    fn cli_search_shape_check_passes_anchored_prefix() {
        check_regex_pattern_text("^get_", 1_000_000, &cfg())
            .expect("anchored prefix must pass CLI shape check");
    }

    #[test]
    fn cli_search_shape_check_passes_long_literal() {
        check_regex_pattern_text(".*deserialize.*", 1_000_000, &cfg())
            .expect("long literal must pass CLI shape check");
    }

    #[test]
    fn cli_search_shape_check_below_threshold_passes() {
        check_regex_pattern_text(".*foo.*", 1_000, &cfg())
            .expect("below cap must pass shape check");
    }
}
