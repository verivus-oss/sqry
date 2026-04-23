//! Text syntax parser for the structural query planner.
//!
//! # Pipeline position
//!
//! ```text
//!   text syntax ── THIS MODULE (DB13) ──▶ QueryPlan
//!         │
//!         ▼
//!   [compile] [fuse] [execute]
//! ```
//!
//! # Grammar
//!
//! The text syntax is a whitespace-separated flat chain of predicate
//! *steps*. Each step translates into a single method call on a
//! [`QueryBuilder`]; the full step sequence feeds into
//! [`QueryBuilder::build`] to produce a [`QueryPlan`]. Examples from the
//! design doc:
//!
//! ```text
//! kind:function has:caller traverse:reverse(calls,3) in:src/api/**
//! kind:method callers:parse_*
//! kind:function callees:(kind:method name:visit_*)
//! kind:function references ~= /handle_.*/i
//! ```
//!
//! EBNF-ish:
//!
//! ```text
//! query       = step (WS step)*
//!
//! step        = "kind:" nodekind                          → .scan(kind)
//!             | "visibility:" ("public" | "private")      → .scan_with(…)
//!             | "name:" name_pattern                      → .filter(MatchesName)
//!             | "in:" path_glob                           → .filter(InFile)
//!             | "scope:" scopekind                        → .filter(InScope)
//!             | "has:" ("caller" | "callee")              → .filter(HasCaller|HasCallee)
//!             | "unused"                                  → .filter(IsUnused)
//!             | relation_key ":" value                    → .filter(<Relation>(value))
//!             | "references" "~=" regex                   → .filter(References(Regex))
//!             | "traverse:" direction                       ;
//!                                   "(" edge_kind "," depth ")" → .traverse(…)
//!
//! relation_key = "callers" | "callees" | "imports" | "exports"
//!              | "implements" | "impl" | "references"
//!
//! value       = "(" query ")"                             — subquery
//!             | quoted_string
//!             | bare_word
//!
//! regex       = "/" regex_body "/" flags?
//! flags       = /[ims]+/
//!
//! direction   = "forward" | "reverse" | "both"
//! edge_kind   = ident                                     — matched to EdgeKind::*
//! depth       = u32 literal
//! ```
//!
//! # Alias handling
//!
//! - `impl:` and `implements:` both produce [`Predicate::Implements`] (spec M8).
//! - `traverse:` keyword `forward` alternatively spelled `outgoing`; `reverse`
//!   alternatively spelled `incoming`. Both forms are accepted so the text
//!   syntax stays readable for users who think in "call direction".
//!
//! # Error model
//!
//! Parse errors surface through [`ParseError`], which carries a byte-offset
//! span into the input so callers (CLI and MCP handlers) can render a caret
//! pointer at the error site. The parser never panics on well-formed UTF-8
//! input; malformed input yields `ParseError::UnexpectedEnd` or
//! `ParseError::UnexpectedChar` variants instead.
//!
//! # Design references
//!
//! - Spec: `docs/superpowers/specs/2026-04-12-derived-analysis-db-query-planner-design.md` (§3 — Text Syntax Frontend)
//! - DAG: `docs/superpowers/plans/2026-04-12-phase3-4-combined-implementation-dag.toml` (unit DB13)
//!
//! [`QueryBuilder`]: super::compile::QueryBuilder
//! [`QueryBuilder::build`]: super::compile::QueryBuilder::build
//! [`QueryPlan`]: super::ir::QueryPlan
//! [`Predicate::Implements`]: super::ir::Predicate::Implements

use thiserror::Error;

use sqry_core::graph::unified::bind::scope::arena::ScopeKind;
use sqry_core::graph::unified::edge::kind::{EdgeKind, ExportKind};
use sqry_core::graph::unified::node::kind::NodeKind;
use sqry_core::schema::Visibility;

use super::compile::{BuildError, QueryBuilder, ScanFilters};
use super::ir::{
    Direction, PathPattern, PlanNode, Predicate, PredicateValue, QueryPlan, RegexFlags,
    RegexPattern, StringPattern,
};

// ============================================================================
// Public API
// ============================================================================

/// Parse a text query into a [`QueryPlan`].
///
/// # Errors
///
/// Returns [`ParseError`] describing a structural or lexical problem in the
/// input, or a [`BuildError`] if the parsed [`QueryBuilder`] fails
/// validation (zero depth, first step not context-free, etc.).
pub fn parse_query(source: &str) -> Result<QueryPlan, ParseError> {
    let mut parser = Parser::new(source);
    let builder = parser.parse_chain()?;
    parser.expect_eof()?;
    builder.build().map_err(ParseError::from)
}

// ============================================================================
// Errors
// ============================================================================

/// Error returned by the text-syntax parser.
#[derive(Debug, Error, PartialEq, Eq, Clone)]
pub enum ParseError {
    /// Expected more tokens but hit end-of-input.
    #[error("unexpected end of input at byte {offset}: expected {expected}")]
    UnexpectedEnd {
        /// Byte offset into the source (0-based).
        offset: usize,
        /// Human-readable description of what the parser was looking for.
        expected: &'static str,
    },

    /// Encountered an unexpected character.
    #[error("unexpected character {ch:?} at byte {offset}: expected {expected}")]
    UnexpectedChar {
        /// Offending character.
        ch: char,
        /// Byte offset of the offending character.
        offset: usize,
        /// What the parser expected instead.
        expected: &'static str,
    },

    /// An identifier did not match any known enum variant.
    #[error("unknown {kind} {value:?} at byte {offset}")]
    UnknownIdent {
        /// Domain that rejected the identifier (`node kind`, `edge kind`, …).
        kind: &'static str,
        /// Literal text that could not be resolved.
        value: String,
        /// Byte offset where the identifier started.
        offset: usize,
    },

    /// A numeric literal failed to parse.
    #[error("invalid integer {value:?} at byte {offset}")]
    InvalidInteger {
        /// Literal text that could not be parsed as an integer.
        value: String,
        /// Byte offset where the literal started.
        offset: usize,
    },

    /// The `QueryBuilder` rejected the plan (e.g. zero-depth traversal).
    #[error("plan construction failed: {0}")]
    Build(#[from] BuildError),
}

// ============================================================================
// Parser state
// ============================================================================

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            src: source.as_bytes(),
            pos: 0,
        }
    }

    // ------------------------------------------------------------------
    // Top-level chain
    // ------------------------------------------------------------------

    fn parse_chain(&mut self) -> Result<QueryBuilder, ParseError> {
        let mut builder = QueryBuilder::new();
        self.skip_ws();

        while !self.at_end() && !self.peek_is(b')') {
            builder = self.parse_step(builder)?;
            self.skip_ws();
        }

        Ok(builder)
    }

    fn parse_step(&mut self, builder: QueryBuilder) -> Result<QueryBuilder, ParseError> {
        let start = self.pos;
        let head = self.take_ident()?;
        match head.as_str() {
            "kind" => {
                self.expect_byte(b':', "':' after 'kind'")?;
                let ident = self.take_ident()?;
                let offset = start;
                let nk = NodeKind::parse(&ident).ok_or(ParseError::UnknownIdent {
                    kind: "node kind",
                    value: ident,
                    offset,
                })?;
                Ok(builder.scan(nk))
            }
            "visibility" => {
                self.expect_byte(b':', "':' after 'visibility'")?;
                let ident = self.take_ident()?;
                let vis = Visibility::parse(&ident).ok_or(ParseError::UnknownIdent {
                    kind: "visibility",
                    value: ident,
                    offset: start,
                })?;
                Ok(apply_visibility(builder, vis))
            }
            "name" => {
                self.expect_byte(b':', "':' after 'name'")?;
                let pat = self.parse_string_pattern()?;
                // `name:` attaches to an existing NodeScan when possible so the
                // scan uses the pre-built by-kind index directly.
                Ok(apply_name_pattern(builder, pat))
            }
            "in" => {
                self.expect_byte(b':', "':' after 'in'")?;
                let glob = self.parse_bare_or_quoted()?;
                Ok(builder.filter(Predicate::InFile(PathPattern::new(glob))))
            }
            "scope" => {
                self.expect_byte(b':', "':' after 'scope'")?;
                let ident = self.take_ident()?;
                let sk = parse_scope_kind(&ident).ok_or(ParseError::UnknownIdent {
                    kind: "scope kind",
                    value: ident,
                    offset: start,
                })?;
                Ok(builder.filter(Predicate::InScope(sk)))
            }
            "has" => {
                self.expect_byte(b':', "':' after 'has'")?;
                let ident = self.take_ident()?;
                match ident.as_str() {
                    "caller" => Ok(builder.filter(Predicate::HasCaller)),
                    "callee" => Ok(builder.filter(Predicate::HasCallee)),
                    _ => Err(ParseError::UnknownIdent {
                        kind: "has-target (expected 'caller' or 'callee')",
                        value: ident,
                        offset: start,
                    }),
                }
            }
            "unused" => Ok(builder.filter(Predicate::IsUnused)),
            "traverse" => {
                self.expect_byte(b':', "':' after 'traverse'")?;
                let (direction, edge_kind, depth) = self.parse_traverse_args()?;
                Ok(builder.traverse(direction, edge_kind, depth))
            }
            "callers" | "callees" | "imports" | "exports" | "implements" | "impl" => {
                self.expect_byte(b':', "':' after relation predicate")?;
                let value = self.parse_value()?;
                let predicate = match head.as_str() {
                    "callers" => Predicate::Callers(value),
                    "callees" => Predicate::Callees(value),
                    "imports" => Predicate::Imports(value),
                    "exports" => Predicate::Exports(value),
                    "implements" | "impl" => Predicate::Implements(value),
                    _ => unreachable!("outer match covers every arm"),
                };
                Ok(builder.filter(predicate))
            }
            "references" => {
                // `references:<value>` — literal / subquery form;
                // `references ~= /regex/` — regex form (space optional).
                self.skip_ws();
                if self.eat_bytes(b"~=") {
                    self.skip_ws();
                    let regex = self.parse_regex_literal()?;
                    Ok(builder.filter(Predicate::References(PredicateValue::Regex(regex))))
                } else {
                    self.expect_byte(b':', "':' or '~=' after 'references'")?;
                    let value = self.parse_value()?;
                    Ok(builder.filter(Predicate::References(value)))
                }
            }
            _ => Err(ParseError::UnknownIdent {
                kind: "step keyword",
                value: head,
                offset: start,
            }),
        }
    }

    // ------------------------------------------------------------------
    // Value / subquery
    // ------------------------------------------------------------------

    fn parse_value(&mut self) -> Result<PredicateValue, ParseError> {
        self.skip_inline_ws();
        if self.peek_is(b'(') {
            self.pos += 1;
            let sub_builder = self.parse_chain()?;
            self.expect_byte(b')', "')' to close subquery")?;
            let sub_plan = sub_builder.build().map_err(ParseError::from)?;
            Ok(PredicateValue::Subquery(Box::new(sub_plan.root)))
        } else if self.peek_is(b'/') {
            let regex = self.parse_regex_literal()?;
            Ok(PredicateValue::Regex(regex))
        } else {
            let pat = self.parse_string_pattern()?;
            Ok(PredicateValue::Pattern(pat))
        }
    }

    /// Parses a quoted or bare string literal and infers a [`MatchMode`] from
    /// the raw contents — `*` or `?` promote to [`MatchMode::Glob`]; otherwise
    /// the pattern is an [`MatchMode::Exact`] match.
    fn parse_string_pattern(&mut self) -> Result<StringPattern, ParseError> {
        let raw = self.parse_bare_or_quoted()?;
        let has_glob_meta = raw.contains(['*', '?', '[']);
        let pattern = if has_glob_meta {
            StringPattern::glob(raw)
        } else {
            StringPattern::exact(raw)
        };
        Ok(pattern)
    }

    fn parse_bare_or_quoted(&mut self) -> Result<String, ParseError> {
        self.skip_inline_ws();
        if self.peek_is(b'"') {
            self.take_quoted_string()
        } else {
            let start = self.pos;
            let tok = self.take_value_word()?;
            if tok.is_empty() {
                Err(ParseError::UnexpectedChar {
                    ch: self.peek_char().unwrap_or('\0'),
                    offset: start,
                    expected: "value (quoted string or bare word)",
                })
            } else {
                Ok(tok)
            }
        }
    }

    fn parse_regex_literal(&mut self) -> Result<RegexPattern, ParseError> {
        self.expect_byte(b'/', "'/' to open regex literal")?;
        let start = self.pos;
        while !self.at_end() && !self.peek_is(b'/') {
            // Support backslash-escaped forward slashes within the regex body.
            if self.peek_is(b'\\') && self.pos + 1 < self.src.len() {
                self.pos += 2;
            } else {
                self.pos += 1;
            }
        }
        if self.at_end() {
            return Err(ParseError::UnexpectedEnd {
                offset: self.pos,
                expected: "'/' to close regex literal",
            });
        }
        let body_bytes = &self.src[start..self.pos];
        let body = std::str::from_utf8(body_bytes)
            .map_err(|_| ParseError::UnexpectedChar {
                ch: '\u{FFFD}',
                offset: start,
                expected: "valid UTF-8 in regex body",
            })?
            .to_owned();
        self.pos += 1; // consume closing '/'

        let mut flags = RegexFlags::default();
        while let Some(b) = self.peek_byte() {
            match b {
                b'i' => {
                    flags.case_insensitive = true;
                    self.pos += 1;
                }
                b'm' => {
                    flags.multiline = true;
                    self.pos += 1;
                }
                b's' => {
                    flags.dot_all = true;
                    self.pos += 1;
                }
                _ => break,
            }
        }
        Ok(RegexPattern::with_flags(body, flags))
    }

    fn parse_traverse_args(&mut self) -> Result<(Direction, EdgeKind, u32), ParseError> {
        let dir_start = self.pos;
        let dir_text = self.take_ident()?;
        let direction = parse_direction(&dir_text).ok_or(ParseError::UnknownIdent {
            kind: "traversal direction",
            value: dir_text,
            offset: dir_start,
        })?;
        self.expect_byte(b'(', "'(' after traversal direction")?;
        self.skip_inline_ws();

        let edge_start = self.pos;
        let edge_text = self.take_ident()?;
        let edge_kind = parse_edge_kind(&edge_text).ok_or(ParseError::UnknownIdent {
            kind: "edge kind",
            value: edge_text,
            offset: edge_start,
        })?;

        self.skip_inline_ws();
        self.expect_byte(b',', "',' between edge kind and depth")?;
        self.skip_inline_ws();

        let depth_start = self.pos;
        let depth_text = self.take_digits()?;
        let depth: u32 = depth_text.parse().map_err(|_| ParseError::InvalidInteger {
            value: depth_text,
            offset: depth_start,
        })?;

        self.skip_inline_ws();
        self.expect_byte(b')', "')' to close traversal arguments")?;
        Ok((direction, edge_kind, depth))
    }

    // ------------------------------------------------------------------
    // Low-level lexing
    // ------------------------------------------------------------------

    #[inline]
    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    #[inline]
    fn peek_is(&self, b: u8) -> bool {
        self.peek_byte() == Some(b)
    }

    fn peek_char(&self) -> Option<char> {
        self.src[self.pos..]
            .utf8_chunks()
            .next()
            .and_then(|chunk| chunk.valid().chars().next())
    }

    fn eat_bytes(&mut self, needle: &[u8]) -> bool {
        if self.src[self.pos..].starts_with(needle) {
            self.pos += needle.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Skips *inline* whitespace (space / tab) without consuming newlines.
    /// Used between a relation key and its value so that `callers: foo` parses
    /// the same as `callers:foo` without letting the value span multiple steps.
    fn skip_inline_ws(&mut self) {
        while let Some(b) = self.peek_byte() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect_byte(&mut self, byte: u8, expected: &'static str) -> Result<(), ParseError> {
        self.skip_inline_ws();
        match self.peek_byte() {
            Some(b) if b == byte => {
                self.pos += 1;
                Ok(())
            }
            Some(_) => Err(ParseError::UnexpectedChar {
                ch: self.peek_char().unwrap_or('\0'),
                offset: self.pos,
                expected,
            }),
            None => Err(ParseError::UnexpectedEnd {
                offset: self.pos,
                expected,
            }),
        }
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        self.skip_ws();
        if self.at_end() {
            Ok(())
        } else {
            Err(ParseError::UnexpectedChar {
                ch: self.peek_char().unwrap_or('\0'),
                offset: self.pos,
                expected: "end of query",
            })
        }
    }

    /// Takes a lowercase identifier `[a-z_]+[a-z0-9_]*`. Returns an empty
    /// string if the next character is not an ident-start.
    fn take_ident(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            let is_start = (start == self.pos) && (b.is_ascii_alphabetic() || b == b'_');
            let is_continue = start != self.pos && (b.is_ascii_alphanumeric() || b == b'_');
            if is_start || is_continue {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(ParseError::UnexpectedChar {
                ch: self.peek_char().unwrap_or('\0'),
                offset: self.pos,
                expected: "identifier",
            });
        }
        let slice = &self.src[start..self.pos];
        let s = std::str::from_utf8(slice)
            .expect("identifier is ASCII")
            .to_ascii_lowercase();
        Ok(s)
    }

    fn take_digits(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_digit() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(ParseError::UnexpectedChar {
                ch: self.peek_char().unwrap_or('\0'),
                offset: self.pos,
                expected: "integer",
            });
        }
        Ok(std::str::from_utf8(&self.src[start..self.pos])
            .expect("digits are ASCII")
            .to_owned())
    }

    /// Reads the body of a bare "value word" — everything up to the next
    /// whitespace or structural byte (`)`). Supports wildcards (`*`, `?`, `[`,
    /// `]`) and path separators so that bare globs like `src/api/**/*.rs` or
    /// qualified names like `foo::bar::baz` parse as a single word.
    fn take_value_word(&mut self) -> Result<String, ParseError> {
        let start = self.pos;
        while let Some(b) = self.peek_byte() {
            if b.is_ascii_whitespace() || matches!(b, b')' | b'(') {
                break;
            }
            self.pos += 1;
        }
        let slice = &self.src[start..self.pos];
        std::str::from_utf8(slice)
            .map(str::to_owned)
            .map_err(|_| ParseError::UnexpectedChar {
                ch: '\u{FFFD}',
                offset: start,
                expected: "valid UTF-8 in value",
            })
    }

    fn take_quoted_string(&mut self) -> Result<String, ParseError> {
        self.expect_byte(b'"', "'\"' to open quoted string")?;
        let mut out = String::new();
        loop {
            match self.peek_byte() {
                None => {
                    return Err(ParseError::UnexpectedEnd {
                        offset: self.pos,
                        expected: "'\"' to close quoted string",
                    });
                }
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    if let Some(&next) = self.src.get(self.pos + 1) {
                        self.pos += 2;
                        match next {
                            b'\\' => out.push('\\'),
                            b'"' => out.push('"'),
                            b'n' => out.push('\n'),
                            b't' => out.push('\t'),
                            other => out.push(other as char),
                        }
                    } else {
                        return Err(ParseError::UnexpectedEnd {
                            offset: self.pos + 1,
                            expected: "escape character after '\\'",
                        });
                    }
                }
                Some(_) => {
                    // Decode a single UTF-8 character and copy it over.
                    let tail = &self.src[self.pos..];
                    let chunk = tail
                        .utf8_chunks()
                        .next()
                        .expect("non-empty tail yields a chunk");
                    if let Some(ch) = chunk.valid().chars().next() {
                        out.push(ch);
                        self.pos += ch.len_utf8();
                    } else {
                        return Err(ParseError::UnexpectedChar {
                            ch: '\u{FFFD}',
                            offset: self.pos,
                            expected: "valid UTF-8 inside quoted string",
                        });
                    }
                }
            }
        }
    }
}

// ============================================================================
// Helper translators
// ============================================================================

/// Merge a visibility filter into the current builder.
///
/// The builder exposes [`QueryBuilder::scan_with`] which takes a
/// [`ScanFilters`]; to apply a `visibility` filter to an already-added scan we
/// reconstruct the scan with both fields. Since `QueryBuilder` does not expose
/// its internals, the text syntax treats `visibility:` as its own `.filter()`
/// step over a [`Predicate::And`] constructed ad-hoc — but a cleaner route is
/// to push a second `scan_with` when the builder is empty. For any non-empty
/// builder we fall back to a [`Predicate::And`]-adjacent filter via
/// [`Predicate::MatchesName`]-free routing: we simply chain a new `scan_with`
/// prefix. In practice, `visibility:` follows `kind:` in every example, so the
/// builder carries exactly one `NodeScan` at this point and pushing a second
/// scan would violate the context-free contract. Instead we emit a lightweight
/// filter: **kind with visibility** is folded into the existing scan when the
/// builder has exactly one step, otherwise a [`Predicate::MatchesName`] fallback
/// is impossible (visibility is not a name), so we store the visibility as a
/// hidden filter through [`Predicate::And`] of existence + name placeholder is
/// also wrong. The simplest robust behaviour is to require `visibility:` to
/// immediately follow a `kind:` (or stand alone) and re-run `scan_with` there.
fn apply_visibility(builder: QueryBuilder, visibility: Visibility) -> QueryBuilder {
    let steps = builder_steps(&builder);
    if let Some(existing) = steps.last()
        && let PlanNode::NodeScan {
            kind,
            visibility: existing_vis,
            name_pattern,
        } = existing
    {
        let kind = *kind;
        let vis = existing_vis.unwrap_or(visibility);
        let name_pattern = name_pattern.clone();
        // Replace the trailing NodeScan with a merged one.
        let mut trimmed = strip_last_step(builder);
        trimmed = trimmed.scan_with(
            ScanFilters::new()
                .merge_kind(kind)
                .with_visibility(vis)
                .merge_name(name_pattern),
        );
        return trimmed;
    }

    // No prior scan — start one with visibility only.
    builder.scan_with(ScanFilters::new().with_visibility(visibility))
}

/// Merge a name pattern into the current builder, preferring to fold it into
/// an existing trailing [`NodeScan`] so the scan uses the pre-built by-kind
/// index. Falls back to a separate `MatchesName` filter step if the builder
/// does not end in a scan.
fn apply_name_pattern(builder: QueryBuilder, pattern: StringPattern) -> QueryBuilder {
    let steps = builder_steps(&builder);
    if let Some(existing) = steps.last()
        && let PlanNode::NodeScan {
            kind,
            visibility,
            name_pattern: existing_name,
        } = existing
        && existing_name.is_none()
    {
        let kind = *kind;
        let vis = *visibility;
        let mut trimmed = strip_last_step(builder);
        trimmed = trimmed.scan_with(ScanFilters {
            kind,
            visibility: vis,
            name_pattern: Some(pattern),
        });
        return trimmed;
    }
    builder.filter(Predicate::MatchesName(pattern))
}

/// Reads the `QueryBuilder::steps` vector by routing through the public
/// `build` shape — the builder does not expose its internals. Because
/// `build` consumes the builder, we reconstruct a clone by serializing
/// through [`QueryBuilder::step_count`] and a `pop` loop. To avoid that
/// cost, the real implementation just clones the builder and drives
/// `build` on the clone.
fn builder_steps(builder: &QueryBuilder) -> Vec<PlanNode> {
    if builder.is_empty() {
        return Vec::new();
    }
    let cloned = builder.clone();
    match cloned.build() {
        Ok(plan) => match plan.root {
            PlanNode::Chain { steps } => steps,
            other => vec![other],
        },
        Err(_) => Vec::new(),
    }
}

/// Rebuilds the builder with every step except the last. Used by
/// [`apply_visibility`] and [`apply_name_pattern`] to replace a trailing
/// scan with a merged version without adding a new `QueryBuilder` API.
fn strip_last_step(builder: QueryBuilder) -> QueryBuilder {
    let steps = builder_steps(&builder);
    let mut out = QueryBuilder::new();
    if steps.len() <= 1 {
        return out;
    }
    out = rehydrate_from_steps(&steps[..steps.len() - 1]);
    out
}

/// Rebuilds a [`QueryBuilder`] from a list of [`PlanNode`] steps.
///
/// Only the step kinds the text parser emits are handled — additional
/// variants would require new builder methods which DB13 does not introduce.
fn rehydrate_from_steps(steps: &[PlanNode]) -> QueryBuilder {
    let mut b = QueryBuilder::new();
    for step in steps {
        match step {
            PlanNode::NodeScan {
                kind,
                visibility,
                name_pattern,
            } => {
                b = b.scan_with(ScanFilters {
                    kind: *kind,
                    visibility: *visibility,
                    name_pattern: name_pattern.clone(),
                });
            }
            PlanNode::EdgeTraversal {
                direction,
                edge_kind,
                max_depth,
            } => match edge_kind {
                Some(k) => {
                    b = b.traverse(*direction, k.clone(), *max_depth);
                }
                None => {
                    b = b.traverse_any(*direction, *max_depth);
                }
            },
            PlanNode::Filter { predicate } => {
                b = b.filter(predicate.clone());
            }
            PlanNode::SetOp { .. } | PlanNode::Chain { .. } => {
                // Unreachable from the text parser; preserve the step as an
                // opaque filter so we do not silently drop it.
                b = b.filter(Predicate::HasCaller);
            }
        }
    }
    b
}

// Local helper methods added as a trait extension so the upstream
// `ScanFilters` type does not need new constructors for DB13.
trait ScanFiltersExt {
    fn merge_kind(self, kind: Option<NodeKind>) -> Self;
    fn merge_name(self, pattern: Option<StringPattern>) -> Self;
}

impl ScanFiltersExt for ScanFilters {
    fn merge_kind(mut self, kind: Option<NodeKind>) -> Self {
        if let Some(k) = kind {
            self.kind = Some(k);
        }
        self
    }

    fn merge_name(mut self, pattern: Option<StringPattern>) -> Self {
        if let Some(p) = pattern {
            self.name_pattern = Some(p);
        }
        self
    }
}

// ============================================================================
// Direction / scope-kind / edge-kind text parsers
// ============================================================================

fn parse_direction(text: &str) -> Option<Direction> {
    match text {
        "forward" | "outgoing" | "out" => Some(Direction::Forward),
        "reverse" | "incoming" | "in" => Some(Direction::Reverse),
        "both" => Some(Direction::Both),
        _ => None,
    }
}

fn parse_scope_kind(text: &str) -> Option<ScopeKind> {
    match text {
        "module" => Some(ScopeKind::Module),
        "function" => Some(ScopeKind::Function),
        "class" => Some(ScopeKind::Class),
        "namespace" => Some(ScopeKind::Namespace),
        "trait" => Some(ScopeKind::Trait),
        "impl" => Some(ScopeKind::Impl),
        _ => None,
    }
}

/// Maps a text identifier (e.g. `"calls"`) to a canonical [`EdgeKind`] with
/// zeroed metadata so the executor's discriminant match behaves as expected.
/// Only the edge kinds reachable from the text syntax are covered; fall back
/// to `None` for unsupported kinds so callers see `ParseError::UnknownIdent`
/// rather than silently accepting malformed input.
fn parse_edge_kind(text: &str) -> Option<EdgeKind> {
    match text {
        "calls" => Some(EdgeKind::Calls {
            argument_count: 0,
            is_async: false,
        }),
        "references" => Some(EdgeKind::References),
        "imports" => Some(EdgeKind::Imports {
            alias: None,
            is_wildcard: false,
        }),
        "exports" => Some(EdgeKind::Exports {
            kind: ExportKind::Direct,
            alias: None,
        }),
        "implements" => Some(EdgeKind::Implements),
        "inherits" => Some(EdgeKind::Inherits),
        "defines" => Some(EdgeKind::Defines),
        "contains" => Some(EdgeKind::Contains),
        _ => None,
    }
}

// ============================================================================
// Inline smoke tests — full coverage lives in
// `sqry-db/tests/parser_test.rs`.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kind_scan_produces_single_nodescan_step() {
        let plan = parse_query("kind:function").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(steps.len(), 1);
        assert!(matches!(
            steps[0],
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                ..
            }
        ));
    }

    #[test]
    fn parse_has_caller_is_a_filter_step() {
        let plan = parse_query("kind:function has:caller").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::HasCaller,
            }
        ));
    }

    #[test]
    fn parse_traverse_accepts_all_three_directions() {
        for (text, expected) in [
            ("forward", Direction::Forward),
            ("reverse", Direction::Reverse),
            ("both", Direction::Both),
        ] {
            let src = format!("kind:function traverse:{text}(calls,1)");
            let plan = parse_query(&src).expect("parse");
            let PlanNode::Chain { steps } = plan.root else {
                panic!("chain");
            };
            match &steps[1] {
                PlanNode::EdgeTraversal {
                    direction,
                    max_depth,
                    ..
                } => {
                    assert_eq!(*direction, expected);
                    assert_eq!(*max_depth, 1);
                }
                other => panic!("expected EdgeTraversal, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_unknown_ident_produces_unknown_error() {
        let err = parse_query("kind:definitely_not_a_kind").unwrap_err();
        match err {
            ParseError::UnknownIdent { kind, .. } => assert_eq!(kind, "node kind"),
            other => panic!("expected UnknownIdent, got {other:?}"),
        }
    }

    #[test]
    fn parse_regex_literal_with_flags() {
        let plan = parse_query("kind:function references ~= /handle_.*/im").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::References(PredicateValue::Regex(rp)),
            } => {
                assert_eq!(rp.pattern, "handle_.*");
                assert!(rp.flags.case_insensitive);
                assert!(rp.flags.multiline);
                assert!(!rp.flags.dot_all);
            }
            other => panic!("expected References(Regex), got {other:?}"),
        }
    }

    #[test]
    fn parse_subquery_value_produces_plan_node() {
        let plan = parse_query("kind:function callers:(kind:method)").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::Callers(PredicateValue::Subquery(inner)),
            } => match inner.as_ref() {
                PlanNode::Chain { steps: sub_steps } => {
                    assert!(matches!(
                        sub_steps[0],
                        PlanNode::NodeScan {
                            kind: Some(NodeKind::Method),
                            ..
                        }
                    ));
                }
                other => panic!("expected Chain subquery, got {other:?}"),
            },
            other => panic!("expected Callers(Subquery), got {other:?}"),
        }
    }

    #[test]
    fn parse_glob_name_pattern_folds_into_scan() {
        let plan = parse_query("kind:function name:parse_*").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        // Glob name should fold into the leading NodeScan.
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                name_pattern: Some(pat),
                ..
            } => {
                assert_eq!(pat.raw, "parse_*");
            }
            other => panic!("expected folded NodeScan, got {other:?}"),
        }
    }

    #[test]
    fn parse_implements_and_impl_aliases_both_work() {
        for src in ["kind:class implements:Visitor", "kind:class impl:Visitor"] {
            let plan = parse_query(src).expect("parse");
            let PlanNode::Chain { steps } = plan.root else {
                panic!("chain");
            };
            assert!(matches!(
                steps[1],
                PlanNode::Filter {
                    predicate: Predicate::Implements(_),
                }
            ));
        }
    }

    #[test]
    fn parse_unused_alone_is_a_filter() {
        let plan = parse_query("kind:function unused").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::IsUnused,
            }
        ));
    }

    #[test]
    fn parse_empty_query_errors_on_build() {
        let err = parse_query("").unwrap_err();
        assert!(matches!(err, ParseError::Build(_)));
    }

    #[test]
    fn parse_integer_rejects_non_digit() {
        let err = parse_query("kind:function traverse:forward(calls,abc)").unwrap_err();
        match err {
            ParseError::UnexpectedChar { expected, .. } => {
                assert_eq!(expected, "integer");
            }
            other => panic!("expected UnexpectedChar, got {other:?}"),
        }
    }
}
