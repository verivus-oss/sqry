//! Go build-constraint parser and canonicaliser (T3.8, Cluster D).
//!
//! This module is a self-contained Rust port of the subset of
//! `go/build/constraint` that sqry needs to recognise file-level build
//! tags and lower them to the canonical infix string stored in
//! [`sqry_core::graph::unified::storage::metadata::MacroNodeMetadata::cfg_condition`].
//!
//! It implements three recognisers, all combined by [`parse_file_prelude`]:
//!
//! 1. The modern `//go:build` form (Go 1.17+) — full boolean grammar
//!    over identifiers with `!`, `&&`, `||`, and parentheses. Precedence
//!    matches `go/build/constraint`: `!` > `&&` > `||`.
//! 2. The legacy `// +build` form (still in widespread use) — space-
//!    separated terms are OR-of-AND; `,` is AND; `!` is negation. When
//!    both forms are present, the `//go:build` line wins and a warning
//!    is logged if the two forms disagree (`01_SPEC` §6.3 AC-T3.8-3).
//! 3. Filename-suffix constraints — `foo_GOOS.go`, `foo_GOARCH.go`,
//!    `foo_GOOS_GOARCH.go`. The `_test` suffix is stripped before
//!    GOOS / GOARCH matching and is NOT itself a build-tag identifier
//!    (`01_SPEC` §6.3 AC-T3.8-6).
//!
//! cgo files (`import "C"`) carry an implicit `cgo` term, conjoined
//! with whatever the other recognisers produce.
//!
//! The output of all parsers is a [`NormalisedExpr`] AST. The single
//! consumer-facing canonical form is produced by
//! [`NormalisedExpr::to_condition_string`], which mirrors the shape that
//! `go/build/constraint`'s `Expr.String()` produces:
//! `"linux && amd64"`, `"!windows"`, `"(linux || darwin) && amd64"`.
//!
//! References:
//! - `01_SPEC` §3.3, §5.3, §6.3 (T3.8 acceptance criteria).
//! - `02_DESIGN` §3.3, §4.3 (algorithm + canonicalisation rules).
//! - <https://pkg.go.dev/go/build/constraint>
//! - <https://pkg.go.dev/cmd/go#hdr-Build_constraints>

use std::path::Path;

/// Maximum recursion depth for `//go:build` parsing.
///
/// Guards against pathological nested-paren inputs (`DoS` surface, per
/// `02_DESIGN` §11.2). 128 levels comfortably exceeds any realistic
/// build-constraint depth — `go/build/constraint` itself has no hard
/// cap but real-world constraints are flat.
const MAX_PARSE_DEPTH: usize = 128;

/// Recognised GOOS values, sourced from `internal/syslist`'s `KnownOS`
/// (Go 1.22+). Update when the Go toolchain adds new platforms.
const KNOWN_GOOS: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "illumos",
    "ios",
    "js",
    "linux",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
];

/// Recognised GOARCH values, sourced from `internal/platform.List`
/// (Go 1.22+). Update when the Go toolchain adds new platforms.
const KNOWN_GOARCH: &[&str] = &[
    "386", "amd64", "arm", "arm64", "loong64", "mips", "mips64", "mips64le", "mipsle", "ppc64",
    "ppc64le", "riscv64", "s390x", "sparc64", "wasm",
];

/// Errors returned by the build-constraint parsers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// Empty input or whitespace-only line.
    Empty,
    /// Unrecognised character or token shape.
    UnexpectedChar(char),
    /// Closing paren missing.
    UnclosedParen,
    /// Operator with no right-hand side (e.g. `linux &&`).
    DanglingOperator,
    /// Recursion exceeded [`MAX_PARSE_DEPTH`] (`DoS` guard).
    DepthExceeded,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty build-constraint expression"),
            Self::UnexpectedChar(c) => write!(f, "unexpected character {c:?} in build constraint"),
            Self::UnclosedParen => write!(f, "unclosed parenthesis in build constraint"),
            Self::DanglingOperator => {
                write!(f, "operator with missing operand in build constraint")
            }
            Self::DepthExceeded => write!(
                f,
                "build-constraint nesting exceeded {MAX_PARSE_DEPTH} levels"
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Canonical AST for a parsed Go build constraint.
///
/// `Flag(s)` is a bare identifier (`linux`, `amd64`, `cgo`, `go1.20`,
/// custom `-tags` values). `Not` / `All` / `Any` correspond to `!`,
/// `&&`, `||` in the modern grammar and to negation / `,` / space in
/// the legacy `// +build` grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NormalisedExpr {
    Flag(String),
    Not(Box<NormalisedExpr>),
    All(Vec<NormalisedExpr>),
    Any(Vec<NormalisedExpr>),
}

impl NormalisedExpr {
    /// Build a `Flag` from a borrowed identifier.
    pub(crate) fn flag(s: &str) -> Self {
        Self::Flag(s.to_string())
    }

    /// Produce the canonical Go-native infix string per `02_DESIGN` §4.3.c.
    ///
    /// Mirrors what `go/build/constraint`'s `Expr.String()` returns:
    /// flat infix, `&&` / `||` with surrounding spaces, `!` prefix
    /// without a space, parentheses only on precedence escalation.
    pub(crate) fn to_condition_string(&self) -> String {
        let mut out = String::new();
        write_canonical(self, 0, &mut out);
        out
    }
}

// Precedence levels for `to_condition_string`. Higher binds tighter.
// `Flag` is a leaf; choosing a value at or above `Not` is irrelevant.
const PREC_ANY: u8 = 0;
const PREC_ALL: u8 = 1;
const PREC_NOT: u8 = 2;
const PREC_FLAG: u8 = 3;

fn write_canonical(node: &NormalisedExpr, parent_prec: u8, out: &mut String) {
    let self_prec = match node {
        NormalisedExpr::Any(_) => PREC_ANY,
        NormalisedExpr::All(_) => PREC_ALL,
        NormalisedExpr::Not(_) => PREC_NOT,
        NormalisedExpr::Flag(_) => PREC_FLAG,
    };
    let needs_parens = self_prec < parent_prec;
    if needs_parens {
        out.push('(');
    }
    match node {
        NormalisedExpr::Flag(s) => out.push_str(s),
        NormalisedExpr::Not(inner) => {
            out.push('!');
            write_canonical(inner, PREC_NOT, out);
        }
        NormalisedExpr::All(items) => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(" && ");
                }
                write_canonical(item, PREC_ALL, out);
            }
        }
        NormalisedExpr::Any(items) => {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(" || ");
                }
                write_canonical(item, PREC_ANY, out);
            }
        }
    }
    if needs_parens {
        out.push(')');
    }
}

// ---------------------------------------------------------------------------
// `//go:build` parser (recursive descent, depth-capped)
// ---------------------------------------------------------------------------

/// Parse a `//go:build` expression body (everything after the
/// `//go:build` directive prefix).
///
/// Implements the grammar:
///
/// ```text
/// expr   := or_expr
/// or     := and ('||' and)*
/// and    := unary ('&&' unary)*
/// unary  := '!' unary | primary
/// primary:= '(' expr ')' | IDENT
/// IDENT  := [A-Za-z_][A-Za-z0-9_.]*       (incl. `go1.20`, `unix`, custom tags)
/// ```
///
/// Recursion is hard-capped at [`MAX_PARSE_DEPTH`]; a deeper input
/// returns [`ParseError::DepthExceeded`].
pub(crate) fn parse_gobuild_expr(src: &str) -> Result<NormalisedExpr, ParseError> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut p = GoBuildParser {
        bytes: trimmed.as_bytes(),
        pos: 0,
        depth: 0,
    };
    let expr = p.parse_or()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return Err(ParseError::UnexpectedChar(p.bytes[p.pos] as char));
    }
    Ok(expr)
}

struct GoBuildParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl GoBuildParser<'_> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            let c = self.bytes[self.pos];
            if c == b' ' || c == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        if self.depth >= MAX_PARSE_DEPTH {
            return Err(ParseError::DepthExceeded);
        }
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_or(&mut self) -> Result<NormalisedExpr, ParseError> {
        self.enter()?;
        let first = self.parse_and()?;
        let mut terms = vec![first];
        loop {
            self.skip_ws();
            if self.peek2(b'|', b'|') {
                self.pos += 2;
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    self.leave();
                    return Err(ParseError::DanglingOperator);
                }
                let next = self.parse_and()?;
                terms.push(next);
            } else {
                break;
            }
        }
        self.leave();
        Ok(if terms.len() == 1 {
            terms.into_iter().next().unwrap()
        } else {
            NormalisedExpr::Any(terms)
        })
    }

    fn parse_and(&mut self) -> Result<NormalisedExpr, ParseError> {
        self.enter()?;
        let first = self.parse_unary()?;
        let mut terms = vec![first];
        loop {
            self.skip_ws();
            if self.peek2(b'&', b'&') {
                self.pos += 2;
                self.skip_ws();
                if self.pos >= self.bytes.len() {
                    self.leave();
                    return Err(ParseError::DanglingOperator);
                }
                let next = self.parse_unary()?;
                terms.push(next);
            } else {
                break;
            }
        }
        self.leave();
        Ok(if terms.len() == 1 {
            terms.into_iter().next().unwrap()
        } else {
            NormalisedExpr::All(terms)
        })
    }

    fn parse_unary(&mut self) -> Result<NormalisedExpr, ParseError> {
        self.enter()?;
        self.skip_ws();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'!' {
            self.pos += 1;
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                self.leave();
                return Err(ParseError::DanglingOperator);
            }
            let inner = self.parse_unary()?;
            self.leave();
            return Ok(NormalisedExpr::Not(Box::new(inner)));
        }
        let p = self.parse_primary()?;
        self.leave();
        Ok(p)
    }

    fn parse_primary(&mut self) -> Result<NormalisedExpr, ParseError> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return Err(ParseError::Empty);
        }
        let c = self.bytes[self.pos];
        if c == b'(' {
            self.pos += 1;
            let inner = self.parse_or()?;
            self.skip_ws();
            if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                return Err(ParseError::UnclosedParen);
            }
            self.pos += 1;
            return Ok(inner);
        }
        if !is_ident_start(c) {
            return Err(ParseError::UnexpectedChar(c as char));
        }
        let start = self.pos;
        while self.pos < self.bytes.len() && is_ident_cont(self.bytes[self.pos]) {
            self.pos += 1;
        }
        let ident = std::str::from_utf8(&self.bytes[start..self.pos]).expect("ASCII ident slice");
        Ok(NormalisedExpr::Flag(ident.to_string()))
    }

    fn peek2(&self, a: u8, b: u8) -> bool {
        self.pos + 1 < self.bytes.len()
            && self.bytes[self.pos] == a
            && self.bytes[self.pos + 1] == b
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'.'
}

// ---------------------------------------------------------------------------
// Legacy `// +build` parser (space-separated OR of comma-separated AND)
// ---------------------------------------------------------------------------

/// Parse a single legacy `// +build` directive body (the text after
/// `// +build ` / `//+build `).
///
/// Per `cmd/go`'s legacy grammar: space-separated terms are OR;
/// comma-separated atoms within a term are AND; a leading `!` on an
/// atom negates it. Parentheses are not part of the legacy grammar.
pub(crate) fn parse_plusbuild_expr(src: &str) -> Result<NormalisedExpr, ParseError> {
    let trimmed = src.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    let mut or_terms: Vec<NormalisedExpr> = Vec::new();
    for term in trimmed.split_ascii_whitespace() {
        let mut and_atoms: Vec<NormalisedExpr> = Vec::new();
        for atom in term.split(',') {
            let atom = atom.trim();
            if atom.is_empty() {
                return Err(ParseError::DanglingOperator);
            }
            let (negated, ident) = if let Some(rest) = atom.strip_prefix('!') {
                (true, rest)
            } else {
                (false, atom)
            };
            if ident.is_empty() {
                return Err(ParseError::DanglingOperator);
            }
            if !ident.bytes().next().is_some_and(is_ident_start)
                || !ident.bytes().all(is_ident_cont)
            {
                return Err(ParseError::UnexpectedChar(
                    ident.chars().next().unwrap_or(' '),
                ));
            }
            let flag = NormalisedExpr::flag(ident);
            and_atoms.push(if negated {
                NormalisedExpr::Not(Box::new(flag))
            } else {
                flag
            });
        }
        let term_expr = if and_atoms.len() == 1 {
            and_atoms.into_iter().next().unwrap()
        } else {
            NormalisedExpr::All(and_atoms)
        };
        or_terms.push(term_expr);
    }
    Ok(if or_terms.len() == 1 {
        or_terms.into_iter().next().unwrap()
    } else {
        NormalisedExpr::Any(or_terms)
    })
}

// ---------------------------------------------------------------------------
// Filename-suffix parser
// ---------------------------------------------------------------------------

/// Parse the GOOS/GOARCH suffix from a Go filename, per cmd/go's
/// filename-matching rules (`01_SPEC` §5.3.c).
///
/// Returns `None` if:
/// - the path's basename does not end in `.go`,
/// - the name has no underscore after the leading non-suffix prefix,
/// - the trailing tokens are not in the known GOOS / GOARCH lists.
///
/// The `_test` suffix is stripped before GOOS/GOARCH matching but is
/// NOT itself a build-tag identifier (AC-T3.8-6). Per cmd/go's
/// `goodOSArchFile`: check the last two tokens for `GOOS_GOARCH`
/// (strict order — `OS` then `ARCH`), then fall back to the last token
/// being either a known GOOS or known GOARCH.
pub(crate) fn parse_filename_suffix(path: &Path) -> Option<NormalisedExpr> {
    let base = path.file_name()?.to_str()?;
    let name = base.strip_suffix(".go")?;
    let name = name.strip_suffix("_test").unwrap_or(name);
    let mut parts = name.split('_');
    let _prefix = parts.next()?;
    let trailing: Vec<&str> = parts.collect();
    if trailing.is_empty() {
        return None;
    }
    let last = *trailing.last()?;
    if trailing.len() >= 2 {
        let second_last = trailing[trailing.len() - 2];
        if is_known_goos(second_last) && is_known_goarch(last) {
            return Some(NormalisedExpr::All(vec![
                NormalisedExpr::flag(second_last),
                NormalisedExpr::flag(last),
            ]));
        }
    }
    if is_known_goos(last) || is_known_goarch(last) {
        return Some(NormalisedExpr::flag(last));
    }
    None
}

fn is_known_goos(s: &str) -> bool {
    KNOWN_GOOS.contains(&s)
}

fn is_known_goarch(s: &str) -> bool {
    KNOWN_GOARCH.contains(&s)
}

// ---------------------------------------------------------------------------
// File-prelude collector
// ---------------------------------------------------------------------------

/// Walk the leading comment block of a Go source file, recognise
/// `//go:build` and legacy `// +build` directives, combine with the
/// filename suffix and any cgo flag, and return the canonical
/// per-file build constraint.
///
/// Returns `None` when the file has no build constraint at all
/// (no `//go:build`, no `// +build`, no recognised filename suffix,
/// and no `import "C"`). Per `01_SPEC` §6.3 AC-T3.8-6 `_test.go` files
/// without an explicit `//go:build` line MUST yield `None`.
///
/// When both `//go:build` and `// +build` are present and disagree
/// (i.e. their canonical forms differ), a warning is logged and
/// `//go:build` wins per cmd/go's authoritative-form rule
/// (`01_SPEC` §6.3 AC-T3.8-3).
pub(crate) fn parse_file_prelude(
    file_text: &[u8],
    filename: &str,
    uses_cgo: bool,
) -> Option<NormalisedExpr> {
    let text = std::str::from_utf8(file_text).ok()?;
    // Strip an optional UTF-8 BOM once at the start of the file: a
    // BOM-prefixed `//go:build` line would otherwise fail the
    // `starts_with("//")` directive guard below and the leading
    // comment-block scan would terminate before any directive is
    // recognised. Per Codex iter-1 BLOCKER-2.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut from_gobuild: Option<NormalisedExpr> = None;
    let mut plusbuild_lines: Vec<NormalisedExpr> = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim_start_matches([' ', '\t']);
        if line.is_empty() {
            continue;
        }
        if !line.starts_with("//") {
            // Hit the first non-comment, non-blank construct
            // (typically `package`); the leading comment block is over.
            break;
        }
        if let Some(rest) = line.strip_prefix("//go:build") {
            // Require whitespace or end after the directive: avoid
            // matching `//go:build_something_else`.
            if rest.is_empty() || rest.chars().next().is_some_and(|c| c == ' ' || c == '\t') {
                if let Ok(expr) = parse_gobuild_expr(rest)
                    && from_gobuild.is_none()
                {
                    from_gobuild = Some(expr);
                }
                continue;
            }
        }
        // Legacy `// +build` or `//+build`.
        let rest_opt = line
            .strip_prefix("// +build")
            .or_else(|| line.strip_prefix("//+build"));
        if let Some(rest) = rest_opt
            && (rest.is_empty() || rest.chars().next().is_some_and(|c| c == ' ' || c == '\t'))
            && let Ok(expr) = parse_plusbuild_expr(rest)
        {
            plusbuild_lines.push(expr);
        }
    }

    let from_plusbuild = match plusbuild_lines.len() {
        0 => None,
        1 => Some(plusbuild_lines.into_iter().next().unwrap()),
        _ => Some(NormalisedExpr::All(plusbuild_lines)),
    };

    let from_lines = match (from_gobuild, from_plusbuild) {
        (Some(g), Some(p)) => {
            if g.to_condition_string() != p.to_condition_string() {
                log::warn!(
                    "build constraint mismatch in {filename}: //go:build = {:?}, // +build = {:?}; //go:build wins",
                    g.to_condition_string(),
                    p.to_condition_string(),
                );
            }
            Some(g)
        }
        (Some(g), None) => Some(g),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    };

    let from_filename = parse_filename_suffix(Path::new(filename));
    let from_cgo = if uses_cgo {
        Some(NormalisedExpr::flag("cgo"))
    } else {
        None
    };

    // Canonical conjoin order: filename suffix first, then build-line
    // directives, then cgo. 01_SPEC §6.3 AC-T3.8-5 pins
    // `cache_linux.go` + `//go:build amd64` → `"linux && amd64"`,
    // i.e. the filename-derived term precedes the build-line term.
    // 02_DESIGN §4.3.a's pseudocode had the reverse order; the AC text
    // takes precedence and the conjoin call is reordered to match.
    // Per Codex iter-1 BLOCKER-1.
    conjoin([from_filename, from_lines, from_cgo])
}

/// Combine 0..3 optional terms with `&&`. Returns `None` if every
/// input is `None`. Flattens single-term and same-precedence nested
/// `All` so the resulting canonical string has no redundant parens.
fn conjoin(terms: impl IntoIterator<Item = Option<NormalisedExpr>>) -> Option<NormalisedExpr> {
    let mut collected: Vec<NormalisedExpr> = Vec::new();
    for t in terms.into_iter().flatten() {
        match t {
            NormalisedExpr::All(items) => collected.extend(items),
            other => collected.push(other),
        }
    }
    match collected.len() {
        0 => None,
        1 => Some(collected.into_iter().next().unwrap()),
        _ => Some(NormalisedExpr::All(collected)),
    }
}

// ---------------------------------------------------------------------------
// Per-file stamping
// ---------------------------------------------------------------------------

/// Walk every staged (non-synthetic) `NodeId` in `staging` and merge a
/// `cfg_condition` `MacroNodeMetadata` onto each. Called by the Go
/// plugin's `build_graph` immediately before it returns, after the
/// file's effective `NormalisedExpr` has been computed by
/// [`parse_file_prelude`] and stringified by
/// [`NormalisedExpr::to_condition_string`].
///
/// Synthetic markers staged by `add_synthetic_variable` and friends
/// are preserved (`02_DESIGN` §4.3.d) — `is_node_synthetic` skips them
/// before any new metadata is constructed so the post-pass never
/// overwrites the synthetic flag via `merge`'s overwrite semantics.
pub(crate) fn stamp_cfg_condition_for_file(
    staging: &mut sqry_core::graph::unified::build::staging::StagingGraph,
    condition: &str,
) {
    use sqry_core::graph::unified::NodeMetadataStore;
    use sqry_core::graph::unified::storage::metadata::MacroNodeMetadata;

    let mut store = NodeMetadataStore::new();
    let mut any = false;
    for node_id in staging.staged_node_ids() {
        if staging.is_node_synthetic(node_id) {
            continue;
        }
        let metadata = MacroNodeMetadata {
            cfg_condition: Some(condition.to_string()),
            ..MacroNodeMetadata::default()
        };
        // Master's U02 metadata reshape: `insert` stores the macro payload
        // as `StoredEntry { typed: Some(Macro(..)), flags: EMPTY }` — the
        // same shape the legacy `NodeMetadata::Macro` variant carried.
        store.insert(node_id, metadata);
        any = true;
    }
    if any {
        staging.merge_macro_metadata(&store);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----- parse_gobuild_expr ------------------------------------------------

    fn parse(s: &str) -> NormalisedExpr {
        parse_gobuild_expr(s).expect("parse")
    }

    #[test]
    fn parse_gobuild_single_ident() {
        assert_eq!(parse("linux"), NormalisedExpr::flag("linux"));
    }

    #[test]
    fn parse_gobuild_and() {
        assert_eq!(
            parse("linux && amd64").to_condition_string(),
            "linux && amd64"
        );
    }

    #[test]
    fn parse_gobuild_or() {
        assert_eq!(
            parse("linux || darwin").to_condition_string(),
            "linux || darwin"
        );
    }

    #[test]
    fn parse_gobuild_not() {
        assert_eq!(parse("!windows").to_condition_string(), "!windows");
    }

    #[test]
    fn parse_gobuild_precedence_and_binds_tighter_than_or() {
        // (linux && amd64) || darwin — no parens needed
        assert_eq!(
            parse("linux && amd64 || darwin").to_condition_string(),
            "linux && amd64 || darwin"
        );
    }

    #[test]
    fn parse_gobuild_explicit_parens_or_under_and() {
        // (linux || darwin) && amd64 — parens required on stringify
        assert_eq!(
            parse("(linux || darwin) && amd64").to_condition_string(),
            "(linux || darwin) && amd64"
        );
    }

    #[test]
    fn parse_gobuild_nested_parens_flat() {
        // (linux && amd64) || (darwin && arm64) — `&&` binds tighter than `||`,
        // so unparenthesised form re-parses to the same tree.
        assert_eq!(
            parse("(linux && amd64) || (darwin && arm64)").to_condition_string(),
            "linux && amd64 || darwin && arm64"
        );
    }

    #[test]
    fn parse_gobuild_not_of_group() {
        assert_eq!(
            parse("!(linux && amd64)").to_condition_string(),
            "!(linux && amd64)"
        );
    }

    #[test]
    fn parse_gobuild_go_version_tag() {
        assert_eq!(parse("go1.20"), NormalisedExpr::flag("go1.20"));
    }

    #[test]
    fn parse_gobuild_depth_cap_rejects_pathological_input() {
        // 200 nested parens — exceeds MAX_PARSE_DEPTH of 128.
        let payload: String = "(".repeat(200) + "linux" + &")".repeat(200);
        assert_eq!(
            parse_gobuild_expr(&payload).err(),
            Some(ParseError::DepthExceeded)
        );
    }

    #[test]
    fn parse_gobuild_rejects_dangling_and() {
        assert_eq!(
            parse_gobuild_expr("linux &&").err(),
            Some(ParseError::DanglingOperator)
        );
    }

    #[test]
    fn parse_gobuild_rejects_unclosed_paren() {
        assert_eq!(
            parse_gobuild_expr("(linux && amd64").err(),
            Some(ParseError::UnclosedParen)
        );
    }

    // ----- parse_plusbuild_expr ----------------------------------------------

    #[test]
    fn parse_plusbuild_single_term_comma_and() {
        // `linux,amd64` → linux && amd64
        assert_eq!(
            parse_plusbuild_expr("linux,amd64")
                .unwrap()
                .to_condition_string(),
            "linux && amd64"
        );
    }

    #[test]
    fn parse_plusbuild_space_separated_terms_or() {
        // `linux darwin` → linux || darwin
        assert_eq!(
            parse_plusbuild_expr("linux darwin")
                .unwrap()
                .to_condition_string(),
            "linux || darwin"
        );
    }

    #[test]
    fn parse_plusbuild_or_of_and() {
        // `linux,amd64 darwin,arm64` → (linux && amd64) || (darwin && arm64)
        assert_eq!(
            parse_plusbuild_expr("linux,amd64 darwin,arm64")
                .unwrap()
                .to_condition_string(),
            "linux && amd64 || darwin && arm64"
        );
    }

    #[test]
    fn parse_plusbuild_negation_prefix() {
        assert_eq!(
            parse_plusbuild_expr("!windows")
                .unwrap()
                .to_condition_string(),
            "!windows"
        );
    }

    // ----- parse_filename_suffix ---------------------------------------------

    #[test]
    fn parse_filename_suffix_goos_only() {
        assert_eq!(
            parse_filename_suffix(Path::new("cache_linux.go"))
                .unwrap()
                .to_condition_string(),
            "linux"
        );
    }

    #[test]
    fn parse_filename_suffix_goarch_only() {
        assert_eq!(
            parse_filename_suffix(Path::new("cache_amd64.go"))
                .unwrap()
                .to_condition_string(),
            "amd64"
        );
    }

    #[test]
    fn parse_filename_suffix_goos_then_goarch() {
        assert_eq!(
            parse_filename_suffix(Path::new("cache_linux_amd64.go"))
                .unwrap()
                .to_condition_string(),
            "linux && amd64"
        );
    }

    #[test]
    fn parse_filename_suffix_strips_test() {
        // `cache_linux_test.go` → "linux" (test stripped first)
        assert_eq!(
            parse_filename_suffix(Path::new("cache_linux_test.go"))
                .unwrap()
                .to_condition_string(),
            "linux"
        );
    }

    #[test]
    fn parse_filename_suffix_test_only_is_none() {
        // `cache_test.go` has no GOOS/GOARCH suffix — AC-T3.8-6.
        assert!(parse_filename_suffix(Path::new("cache_test.go")).is_none());
    }

    #[test]
    fn parse_filename_suffix_plain_is_none() {
        assert!(parse_filename_suffix(Path::new("cache.go")).is_none());
    }

    #[test]
    fn parse_filename_suffix_unknown_token_is_none() {
        assert!(parse_filename_suffix(Path::new("cache_unknown.go")).is_none());
    }

    #[test]
    fn parse_filename_suffix_wrong_order_falls_to_last() {
        // `cache_amd64_linux.go` — amd64 is not GOOS, so the 2-suffix rule
        // doesn't match. Last token is `linux` (GOOS), so the 1-suffix
        // rule produces "linux".
        assert_eq!(
            parse_filename_suffix(Path::new("cache_amd64_linux.go"))
                .unwrap()
                .to_condition_string(),
            "linux"
        );
    }

    // ----- parse_file_prelude ------------------------------------------------

    #[test]
    fn prelude_gobuild_only() {
        let src = b"//go:build linux && amd64\n\npackage cache\n";
        let got = parse_file_prelude(src, "cache.go", false).unwrap();
        assert_eq!(got.to_condition_string(), "linux && amd64");
    }

    #[test]
    fn prelude_plusbuild_only() {
        let src = b"// +build linux,amd64\n\npackage cache\n";
        let got = parse_file_prelude(src, "cache.go", false).unwrap();
        assert_eq!(got.to_condition_string(), "linux && amd64");
    }

    #[test]
    fn prelude_gobuild_wins_when_both_present_and_disagree() {
        // Both present, disagree — `//go:build` wins.
        let src = b"//go:build linux\n// +build darwin\n\npackage cache\n";
        let got = parse_file_prelude(src, "cache.go", false).unwrap();
        assert_eq!(got.to_condition_string(), "linux");
    }

    #[test]
    fn prelude_filename_suffix_only() {
        let src = b"package cache\nfunc f() {}\n";
        let got = parse_file_prelude(src, "cache_linux.go", false).unwrap();
        assert_eq!(got.to_condition_string(), "linux");
    }

    #[test]
    fn prelude_gobuild_and_filename_conjoined() {
        // AC-T3.8-5: `cache_linux.go` + `//go:build amd64` →
        // canonical string MUST be `"linux && amd64"` (filename-first
        // conjoin order per 01_SPEC §6.3 AC-T3.8-5).
        let src = b"//go:build amd64\n\npackage cache\n";
        let got = parse_file_prelude(src, "cache_linux.go", false).unwrap();
        assert_eq!(got.to_condition_string(), "linux && amd64");
    }

    #[test]
    fn prelude_strips_utf8_bom_before_directive_scan() {
        // BOM-prefixed source must still recognise `//go:build`.
        // Per Codex iter-1 BLOCKER-2.
        let mut src: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        src.extend_from_slice(b"//go:build linux\n\npackage cache\n");
        let got = parse_file_prelude(&src, "cache.go", false).unwrap();
        assert_eq!(got.to_condition_string(), "linux");
    }

    #[test]
    fn prelude_test_file_no_constraint() {
        // AC-T3.8-6: `_test.go` without a build line yields None.
        let src = b"package cache\nfunc f() {}\n";
        assert!(parse_file_prelude(src, "cache_test.go", false).is_none());
    }

    #[test]
    fn prelude_cgo_only() {
        // No //go:build, no +build, no filename suffix, but uses_cgo=true.
        let src = b"package cache\nimport \"C\"\n";
        let got = parse_file_prelude(src, "cache.go", true).unwrap();
        assert_eq!(got.to_condition_string(), "cgo");
    }

    #[test]
    fn prelude_no_constraint_is_none() {
        // AC-T3.8-8: plain file with no header / suffix / cgo.
        let src = b"package cache\nfunc f() {}\n";
        assert!(parse_file_prelude(src, "cache.go", false).is_none());
    }

    #[test]
    fn prelude_gobuild_conjoined_with_cgo() {
        let src = b"//go:build linux\n\npackage cache\nimport \"C\"\n";
        let got = parse_file_prelude(src, "cache.go", true).unwrap();
        assert_eq!(got.to_condition_string(), "linux && cgo");
    }

    #[test]
    fn prelude_ignores_lines_after_package() {
        // A `//go:build` line below `package` is NOT a directive — the
        // leading comment block is over once we see `package`.
        let src = b"package cache\n//go:build linux\nfunc f() {}\n";
        assert!(parse_file_prelude(src, "cache.go", false).is_none());
    }

    // ----- to_condition_string round-trip property --------------------------

    #[test]
    fn to_condition_string_round_trip() {
        for input in [
            "linux",
            "linux && amd64",
            "linux || darwin",
            "!windows",
            "linux && amd64 || darwin && arm64",
            "(linux || darwin) && amd64",
            "!(linux && amd64)",
            "go1.20",
        ] {
            let parsed = parse_gobuild_expr(input).unwrap();
            let canonical = parsed.to_condition_string();
            let reparsed = parse_gobuild_expr(&canonical).unwrap();
            assert_eq!(
                parsed.to_condition_string(),
                reparsed.to_condition_string(),
                "round-trip failed for {input:?}"
            );
        }
    }

    #[test]
    fn to_condition_string_flattens_nested_all() {
        // Manually-built nested All — canonicalisation flattens via the
        // `same precedence → no parens` rule.
        let nested = NormalisedExpr::All(vec![
            NormalisedExpr::All(vec![
                NormalisedExpr::flag("linux"),
                NormalisedExpr::flag("amd64"),
            ]),
            NormalisedExpr::flag("cgo"),
        ]);
        assert_eq!(nested.to_condition_string(), "linux && amd64 && cgo");
    }
}
