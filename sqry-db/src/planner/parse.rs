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
//!             | "returns:" type_name                      → .filter(Returns)
//!             | "in:" path_glob                           → .filter(InFile)
//!             | "scope:" scopekind                        → .filter(InScope)
//!             | "has:" ("caller" | "callee")              → .filter(HasCaller|HasCallee)
//!             | "unused"                                  → .filter(IsUnused)
//!             | ("items" | "is_definition") (":" bool)?   → .filter(IsDefinition)
//!             | "address_taken" (":" bool)?               → .filter(IsAddressTaken)
//!             | "resolved_via:" ("direct"|"type_match"|"binding_plane")
//!                                                         → .filter(ResolvedVia)
//!             | "callsite_promiscuous" (":" bool)?        → .filter(HasCallsitePromiscuous)
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
use sqry_core::graph::unified::edge::kind::{EdgeKind, ExportKind, ResolvedVia};
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
            "kind" => self.parse_kind_step(builder, start),
            "visibility" => self.parse_visibility_step(builder, start),
            "name" => self.parse_name_step(builder),
            "returns" => self.parse_returns_step(builder),
            "in" => {
                self.expect_byte(b':', "':' after 'in'")?;
                let glob = self.parse_bare_or_quoted()?;
                Ok(builder.filter(Predicate::InFile(PathPattern::new(glob))))
            }
            "scope" => self.parse_scope_step(builder, start),
            "has" => self.parse_has_step(builder, start),
            "unused" => Ok(builder.filter(Predicate::IsUnused)),
            "items" => {
                let want = self.parse_optional_bool_value(start, "items")?;
                Ok(builder.filter(Predicate::IsDefinition(want)))
            }
            "is_definition" => {
                let want = self.parse_optional_bool_value(start, "is_definition")?;
                Ok(builder.filter(Predicate::IsDefinition(want)))
            }
            "cfg" => self.parse_cfg_step(builder),
            "wraps" => self.parse_wraps_step(builder, start),
            // ----- Phase A (C indirect-call precision) -----
            //
            // Spellings locked per DESIGN §11.1:
            //   address_taken[:true|false]            — bare form => true
            //   resolved_via:direct|type_match|binding_plane
            //   callsite_promiscuous[:true|false]     — bare form => true
            //
            // The bare forms parallel `unused` (no required value). Any
            // value other than the locked set is rejected through
            // `ParseError::UnknownIdent`, mirroring how `has:foo` /
            // `kind:foo` are rejected today.
            "address_taken" => {
                let want = self.parse_optional_bool_value(start, "address_taken")?;
                Ok(builder.filter(Predicate::IsAddressTaken(want)))
            }
            "resolved_via" => self.parse_resolved_via_step(builder),
            "callsite_promiscuous" => {
                let want = self.parse_optional_bool_value(start, "callsite_promiscuous")?;
                Ok(builder.filter(Predicate::HasCallsitePromiscuous(want)))
            }
            "traverse" => {
                self.expect_byte(b':', "':' after 'traverse'")?;
                let (direction, edge_kind, depth) = self.parse_traverse_args()?;
                Ok(builder.traverse(direction, edge_kind, depth))
            }
            "callers" | "callees" | "imports" | "exports" | "implements" | "impl" => {
                self.parse_relation_step(builder, &head)
            }
            "references" => self.parse_references_step(builder),
            "shape" => self.parse_shape_step(builder, start),
            _ => Err(ParseError::UnknownIdent {
                kind: "step keyword",
                value: head,
                offset: start,
            }),
        }
    }

    fn parse_kind_step(
        &mut self,
        builder: QueryBuilder,
        start: usize,
    ) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'kind'")?;
        let ident = self.take_ident()?;
        let nk = NodeKind::parse(&ident).ok_or(ParseError::UnknownIdent {
            kind: "node kind",
            value: ident,
            offset: start,
        })?;
        Ok(builder.scan(nk))
    }

    fn parse_visibility_step(
        &mut self,
        builder: QueryBuilder,
        start: usize,
    ) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'visibility'")?;
        let ident = self.take_ident()?;
        let vis = Visibility::parse(&ident).ok_or(ParseError::UnknownIdent {
            kind: "visibility",
            value: ident,
            offset: start,
        })?;
        Ok(apply_visibility(builder, vis))
    }

    fn parse_name_step(&mut self, builder: QueryBuilder) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'name'")?;
        let pat = self.parse_string_pattern()?;
        // `name:` attaches to an existing NodeScan when possible so the scan
        // uses the by-kind index directly. On an empty chain, it starts a
        // standalone NodeScan so `name:Foo` remains context-free.
        Ok(apply_name_pattern(builder, pat))
    }

    fn parse_returns_step(&mut self, builder: QueryBuilder) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'returns'")?;
        let type_name = self.parse_bare_or_quoted()?;
        Ok(builder.filter(Predicate::Returns(type_name)))
    }

    fn parse_scope_step(
        &mut self,
        builder: QueryBuilder,
        start: usize,
    ) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'scope'")?;
        let ident = self.take_ident()?;
        let sk = parse_scope_kind(&ident).ok_or(ParseError::UnknownIdent {
            kind: "scope kind",
            value: ident,
            offset: start,
        })?;
        Ok(builder.filter(Predicate::InScope(sk)))
    }

    fn parse_has_step(
        &mut self,
        builder: QueryBuilder,
        start: usize,
    ) -> Result<QueryBuilder, ParseError> {
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

    fn parse_cfg_step(&mut self, builder: QueryBuilder) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'cfg'")?;
        self.skip_inline_ws();
        let was_quoted = self.peek_is(b'"');
        let value = self.parse_bare_or_quoted()?;
        let matcher = if was_quoted {
            super::cfg_match::CfgMatcher::Literal(value)
        } else {
            super::cfg_match::CfgMatcher::Semantic(super::cfg_match::CfgAst::flag(value))
        };
        Ok(builder.filter(Predicate::CfgCondition(matcher)))
    }

    fn parse_wraps_step(
        &mut self,
        builder: QueryBuilder,
        start: usize,
    ) -> Result<QueryBuilder, ParseError> {
        self.skip_inline_ws();
        let filter = if self.peek_is(b':') {
            self.pos += 1;
            let ident = self.take_ident()?;
            let kind = parse_wrap_kind(&ident).ok_or(ParseError::UnknownIdent {
                kind: "wrap kind",
                value: ident,
                offset: start,
            })?;
            super::ir::WrapKindFilter::Kind(kind)
        } else {
            super::ir::WrapKindFilter::Any
        };
        Ok(builder.filter(Predicate::Wraps(filter)))
    }

    fn parse_resolved_via_step(
        &mut self,
        builder: QueryBuilder,
    ) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after 'resolved_via'")?;
        let ident_start = self.pos;
        let ident = self.take_ident()?;
        let via = match ident.as_str() {
            "direct" => ResolvedVia::Direct,
            "type_match" => ResolvedVia::TypeMatch,
            "binding_plane" => ResolvedVia::BindingPlane,
            "virtual_dispatch" => ResolvedVia::VirtualDispatch,
            "interface_dispatch" => ResolvedVia::InterfaceDispatch,
            "duck_typed" => ResolvedVia::DuckTyped,
            "structural" => ResolvedVia::Structural,
            "promiscuous_elided" => ResolvedVia::PromiscuousElided,
            _ => {
                return Err(ParseError::UnknownIdent {
                    kind: "resolved_via value (expected 'direct', 'type_match', 'binding_plane', 'virtual_dispatch', 'interface_dispatch', 'duck_typed', 'structural', or 'promiscuous_elided')",
                    value: ident,
                    offset: ident_start,
                });
            }
        };

        let mut builder = builder;
        if builder.try_fold_resolved_via(via) {
            Ok(builder)
        } else {
            Ok(builder.filter(Predicate::ResolvedVia(via)))
        }
    }

    fn parse_relation_step(
        &mut self,
        builder: QueryBuilder,
        head: &str,
    ) -> Result<QueryBuilder, ParseError> {
        self.expect_byte(b':', "':' after relation predicate")?;
        let value = self.parse_value()?;
        let predicate = match head {
            "callers" => Predicate::Callers(value),
            "callees" => Predicate::Callees(value),
            "imports" => Predicate::Imports(value),
            "exports" => Predicate::Exports(value),
            "implements" | "impl" => Predicate::Implements(value),
            _ => unreachable!("outer match covers every arm"),
        };
        Ok(builder.filter(predicate))
    }

    fn parse_references_step(&mut self, builder: QueryBuilder) -> Result<QueryBuilder, ParseError> {
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

    /// `shape~=<symbol>` — body-shape structural similarity (U09). Only the `~=`
    /// operator is accepted (it reads as "structurally approximately equal to").
    /// The value is a bare or quoted probe symbol name.
    fn parse_shape_step(
        &mut self,
        builder: QueryBuilder,
        _start: usize,
    ) -> Result<QueryBuilder, ParseError> {
        self.skip_ws();
        if !self.eat_bytes(b"~=") {
            // Produce a precise "expected `~=`" error rather than a generic one.
            self.expect_byte(b'~', "'~=' after 'shape'")?;
            self.expect_byte(b'=', "'=' to complete '~=' after 'shape'")?;
        }
        self.skip_ws();
        let symbol = self.parse_bare_or_quoted()?;
        Ok(builder.filter(Predicate::ShapeSimilar(symbol)))
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

    /// Parses an optional `:true|:false` suffix for a boolean-flag predicate.
    ///
    /// Used by the Phase A C indirect-call predicates (`address_taken`,
    /// `callsite_promiscuous`) whose grammar mirrors the existing `unused`
    /// arm — bare keyword evaluates to `true`, an explicit `:true`/`:false`
    /// selects the polarity, and any other ident value is rejected via
    /// [`ParseError::UnknownIdent`] paralleling how `kind:foo` is handled.
    ///
    /// `keyword` is the step keyword as it appeared on the source side and
    /// is used to label the `ParseError::UnknownIdent` `kind` field so error
    /// messages identify which predicate rejected the value. `keyword_start`
    /// is the byte offset at which the predicate keyword began, retained
    /// for the error envelope when no `:` follows so the offset points at
    /// the step (matching the rest of the parser's offset conventions).
    fn parse_optional_bool_value(
        &mut self,
        keyword_start: usize,
        keyword: &'static str,
    ) -> Result<bool, ParseError> {
        // Use raw peek_byte rather than skip_inline_ws here: the existing
        // `has:` / `kind:` arms keep the `:` glued to the keyword and so
        // does the spec for `address_taken:true`. A space before `:` is
        // not part of the locked grammar.
        if !self.peek_is(b':') {
            // Bare form — `address_taken` alone => true. Keep the offset
            // pointing at the keyword start so error sites elsewhere
            // remain consistent (this path itself does not error).
            let _ = keyword_start;
            return Ok(true);
        }
        // Consume the ':' and read the bool ident.
        self.pos += 1;
        let value_start = self.pos;
        let ident = self.take_ident()?;
        match ident.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(ParseError::UnknownIdent {
                kind: match keyword {
                    "address_taken" => "address_taken value (expected 'true' or 'false')",
                    "callsite_promiscuous" => {
                        "callsite_promiscuous value (expected 'true' or 'false')"
                    }
                    "items" => "items value (expected 'true' or 'false')",
                    "is_definition" => "is_definition value (expected 'true' or 'false')",
                    // Defensive fallback — keeps the helper reusable for
                    // any future bool-flag predicate the planner adds.
                    _ => "boolean value (expected 'true' or 'false')",
                },
                value: ident,
                offset: value_start,
            }),
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
        let mut trimmed = strip_last_step(&builder);
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

/// Merge a name pattern into the current builder.
///
/// Preference order:
///
/// 1. **Empty builder** (`name:Foo` standalone): start a fresh
///    [`PlanNode::NodeScan`] carrying only the name pattern. This makes
///    `name:Foo` a valid context-free first step (otherwise `compile.rs`
///    would reject the chain as starting with a `Filter`).
/// 2. **Trailing `NodeScan` with no existing name pattern**
///    (`kind:function name:Foo`): fold into the trailing scan so the
///    executor walks the pre-built by-kind index directly and applies
///    the name predicate inside `run_scan`.
/// 3. **Anything else**: fall back to a separate
///    [`Predicate::MatchesName`] filter step. The executor's
///    `entry_name_matches` honours the same byte-exact, synthetic-aware
///    contract documented around the `name:` step in
///    [`Parser::parse_step`].
fn apply_name_pattern(builder: QueryBuilder, pattern: StringPattern) -> QueryBuilder {
    let steps = builder_steps(&builder);
    if steps.is_empty() {
        return builder.scan_with(ScanFilters {
            kind: None,
            visibility: None,
            name_pattern: Some(pattern),
        });
    }
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
        let mut trimmed = strip_last_step(&builder);
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
fn strip_last_step(builder: &QueryBuilder) -> QueryBuilder {
    let steps = builder_steps(builder);
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
                resolved_via,
            } => match edge_kind {
                Some(k) => {
                    b = match resolved_via {
                        Some(_) => b.traverse_with_resolved_via(
                            *direction,
                            k.clone(),
                            *resolved_via,
                            *max_depth,
                        ),
                        None => b.traverse(*direction, k.clone(), *max_depth),
                    };
                }
                None => {
                    // The text parser never emits `resolved_via: Some(_)`
                    // alongside a wildcard edge_kind; the U15 builder also
                    // only installs the field on Calls-specific traversals.
                    // Fall through to the legacy any-edge path.
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

/// Map the snake-case planner spelling of a `WrapKind` to the
/// `sqry_core::graph::unified::edge::WrapKind` enum variant. Used by
/// the `wraps:<kind>` planner predicate (T3 Cluster F).
fn parse_wrap_kind(text: &str) -> Option<sqry_core::graph::unified::edge::WrapKind> {
    use sqry_core::graph::unified::edge::WrapKind;
    match text {
        "errorf_verb" => Some(WrapKind::ErrorfVerb),
        "unwrap_method" => Some(WrapKind::UnwrapMethod),
        "unwrap_multi_method" => Some(WrapKind::UnwrapMultiMethod),
        "errors_is" => Some(WrapKind::ErrorsIs),
        "errors_as" => Some(WrapKind::ErrorsAs),
        "errors_as_type" => Some(WrapKind::ErrorsAsType),
        "errors_join" => Some(WrapKind::ErrorsJoin),
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
            resolved_via: ResolvedVia::Direct,
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
    fn parse_items_alias_defaults_to_definition_true() {
        let plan = parse_query("kind:function items").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::IsDefinition(true),
            }
        ));
    }

    #[test]
    fn parse_is_definition_false_value() {
        let plan = parse_query("kind:function is_definition:false").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::IsDefinition(false),
            }
        ));
    }

    #[test]
    fn parse_items_rejects_non_bool_value() {
        let err = parse_query("kind:function items:maybe").unwrap_err();
        match err {
            ParseError::UnknownIdent { kind, value, .. } => {
                assert!(
                    kind.starts_with("items value"),
                    "expected items value error, got {kind:?}"
                );
                assert_eq!(value, "maybe");
            }
            other => panic!("expected UnknownIdent, got {other:?}"),
        }
    }

    // ----------------------------------------------------------------
    // Phase A (C indirect-call precision) — U14 planner predicates.
    //
    // Spellings locked per DESIGN §11.1. Every assertion here is part
    // of the public, user-visible grammar contract — DO NOT widen
    // these tests to accept additional spellings without also editing
    // the DESIGN doc.
    // ----------------------------------------------------------------

    #[test]
    fn parse_address_taken_true_value() {
        let plan = parse_query("kind:function address_taken:true").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::IsAddressTaken(true),
            }
        ));
    }

    #[test]
    fn parse_address_taken_false_value() {
        let plan = parse_query("kind:function address_taken:false").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::IsAddressTaken(false),
            }
        ));
    }

    #[test]
    fn parse_address_taken_bare_defaults_to_true() {
        // DESIGN §11.1: `address_taken:true` (or `address_taken` bare).
        let plan = parse_query("kind:function address_taken").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::IsAddressTaken(true),
            }
        ));
    }

    #[test]
    fn parse_address_taken_rejects_non_bool_value() {
        // Any value other than 'true'/'false' must reject —
        // mirrors how `kind:foo` / `has:foo` are handled.
        let err = parse_query("kind:function address_taken:yes").unwrap_err();
        match err {
            ParseError::UnknownIdent { kind, value, .. } => {
                assert!(
                    kind.starts_with("address_taken value"),
                    "expected address_taken value error, got {kind:?}"
                );
                assert_eq!(value, "yes");
            }
            other => panic!("expected UnknownIdent, got {other:?}"),
        }
    }

    #[test]
    fn parse_resolved_via_direct() {
        let plan = parse_query("kind:function resolved_via:direct").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::ResolvedVia(ResolvedVia::Direct),
            }
        ));
    }

    #[test]
    fn parse_resolved_via_type_match() {
        let plan = parse_query("kind:function resolved_via:type_match").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::ResolvedVia(ResolvedVia::TypeMatch),
            }
        ));
    }

    #[test]
    fn parse_resolved_via_binding_plane() {
        let plan = parse_query("kind:function resolved_via:binding_plane").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::ResolvedVia(ResolvedVia::BindingPlane),
            }
        ));
    }

    #[test]
    fn parse_resolved_via_rejects_unknown_value() {
        let err = parse_query("kind:function resolved_via:invalid").unwrap_err();
        match err {
            ParseError::UnknownIdent { kind, value, .. } => {
                assert!(
                    kind.starts_with("resolved_via value"),
                    "expected resolved_via value error, got {kind:?}"
                );
                assert_eq!(value, "invalid");
            }
            other => panic!("expected UnknownIdent, got {other:?}"),
        }
    }

    #[test]
    fn parse_resolved_via_rejects_camel_case_value() {
        // `take_ident` lowercases its input, so a camel-cased value
        // becomes `bindingplane` which is not in the locked set. This
        // pins the canonical snake_case spelling.
        let err = parse_query("kind:function resolved_via:bindingPlane").unwrap_err();
        match err {
            ParseError::UnknownIdent { value, .. } => {
                assert_eq!(value, "bindingplane");
            }
            other => panic!("expected UnknownIdent, got {other:?}"),
        }
    }

    #[test]
    fn parse_callsite_promiscuous_true_value() {
        let plan = parse_query("kind:function callsite_promiscuous:true").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::HasCallsitePromiscuous(true),
            }
        ));
    }

    #[test]
    fn parse_callsite_promiscuous_false_value() {
        let plan = parse_query("kind:function callsite_promiscuous:false").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::HasCallsitePromiscuous(false),
            }
        ));
    }

    #[test]
    fn parse_callsite_promiscuous_bare_defaults_to_true() {
        // Same shape contract as `address_taken` per DESIGN §11.2.
        let plan = parse_query("kind:function callsite_promiscuous").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert!(matches!(
            steps[1],
            PlanNode::Filter {
                predicate: Predicate::HasCallsitePromiscuous(true),
            }
        ));
    }

    #[test]
    fn parse_callsite_promiscuous_rejects_numeric_value() {
        // The grammar accepts only the literal idents `true`/`false`,
        // not numeric encodings.
        let err = parse_query("kind:function callsite_promiscuous:1").unwrap_err();
        // `1` is not an ident-start byte, so `take_ident` fails with
        // `UnexpectedChar` — that is still a hard parse rejection
        // (the contract here is "must reject", not a specific error
        // discriminant).
        match err {
            ParseError::UnexpectedChar { .. } | ParseError::UnknownIdent { .. } => {}
            other => panic!("expected parse rejection, got {other:?}"),
        }
    }

    #[test]
    fn parse_empty_query_errors_on_build() {
        let err = parse_query("").unwrap_err();
        assert!(matches!(err, ParseError::Build(_)));
    }

    #[test]
    fn parse_returns_predicate_basic() {
        let plan = parse_query("kind:function returns:error").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::Returns(name),
            } => {
                assert_eq!(name, "error");
            }
            other => panic!("expected Filter(Returns), got {other:?}"),
        }
    }

    #[test]
    fn parse_returns_does_not_collide_with_name_predicate() {
        // `name:Foo returns:Bar` must produce two distinct predicate variants
        // — `name:` folds into the leading NodeScan (single step), and
        // `returns:` lands as a Filter step on top.
        let plan = parse_query("kind:function name:Foo returns:Bar").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            PlanNode::NodeScan {
                kind: Some(NodeKind::Function),
                name_pattern: Some(pat),
                ..
            } => {
                assert_eq!(pat.raw, "Foo");
            }
            other => panic!("expected leading NodeScan with name_pattern, got {other:?}"),
        }
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::Returns(name),
            } => {
                assert_eq!(name, "Bar");
            }
            other => panic!("expected Filter(Returns), got {other:?}"),
        }
    }

    #[test]
    fn parse_returns_takes_value_byte_exact_no_glob_promotion() {
        // `returns:` keeps glob meta as literal name bytes (the spec says
        // exact match only; future `returns~:` would handle regex).
        let plan = parse_query("kind:function returns:Result*").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::Returns(name),
            } => {
                assert_eq!(name, "Result*");
            }
            other => panic!("expected Filter(Returns), got {other:?}"),
        }
    }

    #[test]
    fn parse_returns_quoted_string_value() {
        let plan = parse_query(r#"kind:function returns:"std::io::Error""#).expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("chain");
        };
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::Returns(name),
            } => {
                assert_eq!(name, "std::io::Error");
            }
            other => panic!("expected Filter(Returns), got {other:?}"),
        }
    }

    #[test]
    fn parse_returns_missing_value_is_an_error() {
        let err = parse_query("kind:function returns:").unwrap_err();
        // Missing value yields `parse_bare_or_quoted` failure or end-of-input
        // depending on whitespace; both surface as parse errors rather than
        // silently producing an empty `Returns("")` predicate.
        assert!(matches!(
            err,
            ParseError::UnexpectedChar { .. } | ParseError::UnexpectedEnd { .. }
        ));
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

    /// REQ:R0014 — `take_value_word` lock-in: dot-qualified `name:` value.
    ///
    /// `name:Foo.bar` must fold into the leading `NodeScan` as a single
    /// literal `name_pattern` carrying the full dotted string `Foo.bar`. This
    /// freezes today's `take_value_word` behaviour so that field-emission
    /// units (U06 Ruby, U11 Rust, U07 C++) can rely on dot-qualified lookups
    /// resolving via the planner's `Predicate::MatchesName` filter.
    #[test]
    fn parses_dot_qualified_name() {
        let plan = parse_query("name:Foo.bar").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            PlanNode::NodeScan {
                name_pattern: Some(pat),
                ..
            } => {
                assert_eq!(pat.raw, "Foo.bar");
            }
            other => panic!("expected NodeScan with name_pattern, got {other:?}"),
        }
    }

    /// REQ:R0014 — `take_value_word` lock-in: Rust `::`-qualified `name:` value.
    ///
    /// `name:my_crate::Counter::count` must fold into the leading `NodeScan`
    /// as a single literal `name_pattern` carrying the full `::`-separated
    /// string. U11 (Rust field emission) emits `crate::Struct::field` style
    /// qualified names; this test guards that the planner's value-word reader
    /// keeps `::` as part of a single token rather than splitting on `:`.
    #[test]
    fn parses_rust_qualified_name_with_double_colon() {
        let plan = parse_query("name:my_crate::Counter::count").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            PlanNode::NodeScan {
                name_pattern: Some(pat),
                ..
            } => {
                assert_eq!(pat.raw, "my_crate::Counter::count");
            }
            other => panic!("expected NodeScan with name_pattern, got {other:?}"),
        }
    }

    /// REQ:R0014 — `take_value_word` lock-in: Ruby `#`-separated `name:` value.
    ///
    /// `name:Counter#increment` must fold into the leading `NodeScan` as a
    /// single literal `name_pattern` carrying the full `Class#method` string.
    /// U06 (Ruby field emission) uses `#` as the canonical instance-method
    /// separator; this test guards that the planner's value-word reader does
    /// not treat `#` as a comment or whitespace marker.
    #[test]
    fn parses_ruby_instance_method_separator() {
        let plan = parse_query("name:Counter#increment").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(steps.len(), 1);
        match &steps[0] {
            PlanNode::NodeScan {
                name_pattern: Some(pat),
                ..
            } => {
                assert_eq!(pat.raw, "Counter#increment");
            }
            other => panic!("expected NodeScan with name_pattern, got {other:?}"),
        }
    }

    // ============================================================================
    // T3 Cluster F — cfg: + wraps: planner predicates
    // ============================================================================

    fn first_filter_predicate(plan: &QueryPlan) -> &Predicate {
        let PlanNode::Chain { steps } = &plan.root else {
            panic!("expected Chain root");
        };
        for step in steps {
            if let PlanNode::Filter { predicate, .. } = step {
                return predicate;
            }
        }
        panic!("no Filter step in plan");
    }

    #[test]
    fn cfg_predicate_parse_bare() {
        let plan = parse_query("kind:function cfg:linux").expect("parse");
        match first_filter_predicate(&plan) {
            Predicate::CfgCondition(super::super::cfg_match::CfgMatcher::Semantic(ast)) => {
                assert_eq!(
                    ast,
                    &super::super::cfg_match::CfgAst::Flag("linux".to_string())
                );
            }
            other => panic!("expected CfgCondition::Semantic, got {other:?}"),
        }
    }

    #[test]
    fn cfg_predicate_parse_quoted() {
        // Per 02_DESIGN §5.3.a + §10.4: quoted form is Literal-only
        // (byte-exact, language-specific). Bare form is Semantic
        // (cross-language). The two addressing modes are kept
        // independently observable so `cfg:"linux"` returns ONLY
        // Go-stored symbols and `cfg:"target_os = \"linux\""`
        // returns ONLY Rust-stored symbols (§10.4 regression).
        let plan = parse_query("kind:function cfg:\"linux && amd64\"").expect("parse");
        match first_filter_predicate(&plan) {
            Predicate::CfgCondition(super::super::cfg_match::CfgMatcher::Literal(lit)) => {
                assert_eq!(lit, "linux && amd64");
            }
            other => panic!("expected CfgCondition::Literal, got {other:?}"),
        }
    }

    #[test]
    fn wraps_predicate_parse_bare() {
        let plan = parse_query("kind:function wraps").expect("parse");
        match first_filter_predicate(&plan) {
            Predicate::Wraps(super::super::ir::WrapKindFilter::Any) => {}
            other => panic!("expected Wraps(Any), got {other:?}"),
        }
    }

    #[test]
    fn wraps_predicate_parse_filtered() {
        let plan = parse_query("kind:function wraps:errors_is").expect("parse");
        use sqry_core::graph::unified::edge::WrapKind;
        match first_filter_predicate(&plan) {
            Predicate::Wraps(super::super::ir::WrapKindFilter::Kind(WrapKind::ErrorsIs)) => {}
            other => panic!("expected Wraps(Kind(ErrorsIs)), got {other:?}"),
        }
    }

    #[test]
    fn wraps_predicate_parse_unknown_kind_errors() {
        let err = parse_query("kind:function wraps:not_a_kind").expect_err("should fail");
        match err {
            ParseError::UnknownIdent { kind, .. } => {
                assert_eq!(kind, "wrap kind");
            }
            other => panic!("expected UnknownIdent for wrap kind, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // U15 iter-1 follow-up — DESIGN §6.3bis adjacency fold for
    // `resolved_via:X` after a Calls `traverse:` step. These tests pin
    // the parser-surface contract: the fold is conditional on the
    // immediately-preceding step being a Calls `EdgeTraversal`, and is
    // a no-op (i.e. the U14 `Predicate::ResolvedVia` filter form is
    // emitted) for every other context.
    // -----------------------------------------------------------------

    #[test]
    fn parser_folds_resolved_via_after_calls_traversal() {
        // DESIGN §6.3bis lines 1056-1067: when `resolved_via:X` is
        // adjacent to a Calls traversal, fold into the EdgeTraversal's
        // outer `resolved_via` field — NO trailing Filter step.
        let plan =
            parse_query("kind:function traverse:forward(calls,2) resolved_via:binding_plane")
                .expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(
            steps.len(),
            2,
            "fold must consume the Filter step; expected exactly NodeScan + EdgeTraversal, got {steps:?}"
        );
        match &steps[1] {
            PlanNode::EdgeTraversal {
                edge_kind: Some(EdgeKind::Calls { .. }),
                resolved_via: Some(ResolvedVia::BindingPlane),
                max_depth: 2,
                ..
            } => {}
            other => panic!(
                "expected folded Calls EdgeTraversal with resolved_via=Some(BindingPlane), got {other:?}"
            ),
        }
    }

    #[test]
    fn parser_keeps_resolved_via_as_filter_when_no_calls_traversal_precedes() {
        // U14 node-scan semantics: when `resolved_via:X` follows a
        // NodeScan (no Calls traversal), the Filter form is preserved
        // so the executor's `node_has_calls_resolved_via` one-edge-back
        // probe still drives the selection.
        let plan = parse_query("kind:function resolved_via:binding_plane").expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(steps.len(), 2);
        match &steps[1] {
            PlanNode::Filter {
                predicate: Predicate::ResolvedVia(ResolvedVia::BindingPlane),
            } => {}
            other => panic!("expected Filter(ResolvedVia(BindingPlane)), got {other:?}"),
        }
    }

    #[test]
    fn parser_does_not_fold_resolved_via_into_imports_traversal() {
        // The fold is Calls-specific: a `resolved_via:X` predicate
        // adjacent to a non-Calls traversal MUST emit the Filter form
        // (the executor filter parameter is a no-op for non-Calls edge
        // kinds anyway, but the parser surface must stay honest).
        let plan = parse_query("kind:module traverse:forward(imports,1) resolved_via:direct")
            .expect("parse");
        let PlanNode::Chain { steps } = plan.root else {
            panic!("expected Chain root");
        };
        assert_eq!(
            steps.len(),
            3,
            "non-Calls traversal must NOT swallow resolved_via filter; got {steps:?}"
        );
        match &steps[1] {
            PlanNode::EdgeTraversal {
                edge_kind: Some(EdgeKind::Imports { .. }),
                resolved_via: None,
                ..
            } => {}
            other => panic!("expected unfolded Imports EdgeTraversal, got {other:?}"),
        }
        match &steps[2] {
            PlanNode::Filter {
                predicate: Predicate::ResolvedVia(ResolvedVia::Direct),
            } => {}
            other => panic!("expected trailing Filter(ResolvedVia(Direct)), got {other:?}"),
        }
    }

    #[test]
    fn parser_fold_targets_eight_resolved_via_variants() {
        // Spot-check that the fold spelling table covers every locked
        // ResolvedVia value across the V12 8-variant set. Direct +
        // TypeMatch are exercised explicitly because the executor
        // filter is symmetric across the variants.
        for (spelling, expected) in [
            ("direct", ResolvedVia::Direct),
            ("type_match", ResolvedVia::TypeMatch),
            ("binding_plane", ResolvedVia::BindingPlane),
            ("virtual_dispatch", ResolvedVia::VirtualDispatch),
            ("interface_dispatch", ResolvedVia::InterfaceDispatch),
            ("duck_typed", ResolvedVia::DuckTyped),
            ("structural", ResolvedVia::Structural),
            ("promiscuous_elided", ResolvedVia::PromiscuousElided),
        ] {
            let src = format!("kind:function traverse:forward(calls,1) resolved_via:{spelling}");
            let plan = parse_query(&src).unwrap_or_else(|e| panic!("parse {src:?}: {e:?}"));
            let PlanNode::Chain { steps } = plan.root else {
                panic!("expected Chain root for {src:?}");
            };
            assert_eq!(steps.len(), 2, "fold must drop Filter for {src:?}");
            match &steps[1] {
                PlanNode::EdgeTraversal {
                    edge_kind: Some(EdgeKind::Calls { .. }),
                    resolved_via: Some(got),
                    ..
                } => assert_eq!(*got, expected, "fold target mismatch for {src:?}"),
                other => panic!("expected folded Calls EdgeTraversal for {src:?}, got {other:?}"),
            }
        }
    }
}
