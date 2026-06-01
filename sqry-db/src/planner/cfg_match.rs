//! Cross-language `cfg:` predicate comparator — T3 Cluster F.
//!
//! Bridges the two plugin-native shapes the unified graph stores in
//! `MacroNodeMetadata::cfg_condition`:
//!
//! - **Go-native infix** (sqry-lang-go's
//!   `relations::build_constraints::NormalisedExpr::to_condition_string`):
//!   `"linux"`, `"linux && amd64"`, `"!windows"`, `"(linux || darwin) && amd64"`.
//! - **Rust-functional** (sqry-lang-rust's
//!   `macro_boundaries::cfg_analysis::CfgPredicate::to_condition_string`):
//!   `"unix"`, `"target_os = \"linux\""`, `"all(unix, target_arch = \"x86_64\")"`,
//!   `"not(test)"`.
//!
//! 02_DESIGN §5.3.a locks the contract:
//!
//! 1. Stored strings remain plugin-native — neither plugin rewrites the
//!    other's shape on insertion (01_SPEC §6.3 ACs hold byte-for-byte).
//! 2. Cross-language matching at query time goes through a single
//!    comparator that parses both shapes into a shared [`CfgAst`] and
//!    compares semantically (set-equality for `All`/`Any`, identity for
//!    `Flag` after normalising the well-known platform-token
//!    synonyms).
//!
//! References:
//! - 01_SPEC.md §6.3 (T3.8 acceptance criteria).
//! - 02_DESIGN.md §5.3.a (this module's algorithm), §2.3 (storage
//!   contract on `cfg_condition`).
//! - 03_IMPLEMENTATION_PLAN.md §Cluster F.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Cross-language conditional-compilation AST. Both Go's infix grammar
/// and Rust's functional grammar lower into this shared shape; the
/// stored strings are never normalised, only their parsed ASTs.
///
/// `semantically_equals` first canonicalises both sides (dedup
/// operand lists, collapse single-operand `All([x])` / `Any([x])`
/// to `x`) and then compares structurally with **set-equality** on
/// `All` / `Any` operand lists (operand order is irrelevant per
/// 02_DESIGN §5.3.a). `Flag` is identity-compared after the
/// platform-token normalisation in [`normalise_flag`] (so Rust
/// `target_os = "linux"` and Go `linux` both reduce to
/// `Flag("linux")`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CfgAst {
    /// A bare flag (e.g. `linux`, `unix`, `cgo`, `target_os=linux`).
    /// The string is the post-normalisation token.
    Flag(String),
    /// Negation.
    Not(Box<CfgAst>),
    /// Logical AND. Compared by set equality after canonicalisation
    /// (duplicates collapse, operand order does not matter).
    All(Vec<CfgAst>),
    /// Logical OR. Compared by set equality after canonicalisation.
    Any(Vec<CfgAst>),
}

impl CfgAst {
    /// Bare-flag constructor.
    pub fn flag(s: impl Into<String>) -> Self {
        Self::Flag(normalise_flag(&s.into()))
    }

    /// Returns `true` when this AST is structurally and semantically
    /// equivalent to `other` under the design's **set-equality** rule
    /// (02_DESIGN §5.3.a). Both sides are first canonicalised:
    ///
    /// 1. Recursively canonicalise each operand.
    /// 2. Dedup operand lists for `All` / `Any` (`linux && linux`
    ///    collapses to `linux`).
    /// 3. Single-operand `All([x])` / `Any([x])` collapse to `x`.
    ///
    /// After canonicalisation, equality is structural with
    /// order-invariant set comparison for `All` / `Any`.
    ///
    /// The dedup rule is load-bearing for Cluster D's redundant-but-
    /// legal stored shapes — a Go file with filename suffix
    /// `*_linux.go` AND `//go:build linux` flows through
    /// `conjoin` to `All([linux, linux])` (the Go combiner does not
    /// dedup); `cfg:linux` (a single-flag matcher) must still match
    /// that stored AST.
    pub fn semantically_equals(&self, other: &CfgAst) -> bool {
        let a = self.canonical();
        let b = other.canonical();
        struct_equals(&a, &b)
    }

    /// Canonicalise the AST: recurse into operands, dedup operand
    /// lists, and collapse single-operand compounds.
    fn canonical(&self) -> CfgAst {
        match self {
            CfgAst::Flag(_) => self.clone(),
            CfgAst::Not(inner) => CfgAst::Not(Box::new(inner.canonical())),
            CfgAst::All(items) => canonical_compound(items, /*is_all=*/ true),
            CfgAst::Any(items) => canonical_compound(items, /*is_all=*/ false),
        }
    }
}

fn canonical_compound(items: &[CfgAst], is_all: bool) -> CfgAst {
    let mut uniq: Vec<CfgAst> = Vec::new();
    for it in items {
        let canonical = it.canonical();
        // Flatten same-kind nesting: `All([Flag(linux), All([Flag(amd64),
        // Flag(cgo)])])` collapses to `All([linux, amd64, cgo])` so the
        // Rust functional shape `all(target_os = "linux", all(
        // target_arch = "amd64", target_feature = "cgo"))` (or any
        // language source that produces same-kind nesting) compares
        // equal to its flat form under set-equality. Without this,
        // semantically equivalent Rust cfg ASTs that nest `all(...)`
        // inside `all(...)` (or `any(...)` inside `any(...)`) miss
        // their match. Codex multi-LLM iter-1 finding 3.
        let nested_same_kind = matches!(
            (&canonical, is_all),
            (CfgAst::All(_), true) | (CfgAst::Any(_), false)
        );
        if nested_same_kind {
            let inner_items = match canonical {
                CfgAst::All(xs) | CfgAst::Any(xs) => xs,
                _ => unreachable!(),
            };
            for inner in inner_items {
                if !uniq.iter().any(|u| struct_equals(u, &inner)) {
                    uniq.push(inner);
                }
            }
        } else if !uniq.iter().any(|u| struct_equals(u, &canonical)) {
            uniq.push(canonical);
        }
    }
    if uniq.len() == 1 {
        uniq.into_iter().next().unwrap()
    } else if is_all {
        CfgAst::All(uniq)
    } else {
        CfgAst::Any(uniq)
    }
}

/// Structural equality on **already-canonicalised** ASTs (operand
/// lists are deduplicated, single-operand compounds collapsed).
/// `All` / `Any` use order-invariant set equality.
fn struct_equals(a: &CfgAst, b: &CfgAst) -> bool {
    match (a, b) {
        (CfgAst::Flag(x), CfgAst::Flag(y)) => x == y,
        (CfgAst::Not(x), CfgAst::Not(y)) => struct_equals(x, y),
        (CfgAst::All(xs), CfgAst::All(ys)) => set_equals(xs, ys),
        (CfgAst::Any(xs), CfgAst::Any(ys)) => set_equals(xs, ys),
        _ => false,
    }
}

/// Set-equality for canonicalised operand lists: same length, and
/// every `x` has a structural match somewhere in `ys`. Because both
/// sides are canonicalised (no duplicates), this is equivalent to
/// set inclusion in both directions.
fn set_equals(xs: &[CfgAst], ys: &[CfgAst]) -> bool {
    if xs.len() != ys.len() {
        return false;
    }
    xs.iter().all(|x| ys.iter().any(|y| struct_equals(x, y)))
}

/// Query-side matcher: matches a stored `cfg_condition` string.
///
/// Per 02_DESIGN §5.3.a + §10.4 the planner distinguishes two
/// addressing modes:
///
/// - [`CfgMatcher::Semantic`] — bare planner tokens (`cfg:linux`)
///   route the cross-language comparator: a single Go-form `linux`
///   AST matches Go-side `"linux"`, Rust-side
///   `"target_os = \"linux\""`, and compound positive expressions that
///   contain that flag (for example `"linux && amd64"`).
/// - [`CfgMatcher::Literal`] — quoted planner forms
///   (`cfg:"linux && amd64"`, `cfg:"target_os = \"linux\""`) are
///   byte-exact and stay language-specific. The 02_DESIGN §10.4
///   regression locks this contract: `cfg:"linux"` returns ONLY
///   Go-stored symbols and `cfg:"target_os = \"linux\""` returns
///   ONLY Rust-stored symbols.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CfgMatcher {
    /// Semantic match: parse `stored` then compare semantically. A
    /// single-flag query uses positive containment (`cfg:linux` matches
    /// `linux && amd64`); compound queries use exact semantic equality.
    Semantic(CfgAst),
    /// Byte-exact match against `stored`.
    Literal(String),
}

/// Returns `true` iff `stored` matches `matcher`.
pub fn matches_stored(matcher: &CfgMatcher, stored: &str) -> bool {
    match matcher {
        CfgMatcher::Literal(q) => stored == q,
        CfgMatcher::Semantic(q) => semantic_match(&parse_stored_cfg(stored), q),
    }
}

fn semantic_match(stored: &CfgAst, query: &CfgAst) -> bool {
    let stored = stored.canonical();
    let query = query.canonical();
    match &query {
        CfgAst::Flag(_) => contains_positive_flag(&stored, &query),
        _ => struct_equals(&stored, &query),
    }
}

fn contains_positive_flag(stored: &CfgAst, query_flag: &CfgAst) -> bool {
    match stored {
        CfgAst::Flag(_) => struct_equals(stored, query_flag),
        CfgAst::All(items) | CfgAst::Any(items) => items
            .iter()
            .any(|item| contains_positive_flag(item, query_flag)),
        CfgAst::Not(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Parser dispatch
// ---------------------------------------------------------------------------

/// Auto-detect which plugin produced `stored` and parse into [`CfgAst`].
///
/// Detection heuristic per 02_DESIGN §5.3.a:
/// - Presence of `all(` / `any(` / `not(` / ` = ` ⇒ Rust-functional.
/// - Presence of `&&` / `||` / `!` infix (and absent of the above)
///   ⇒ Go-native.
/// - Otherwise (bare ident) ⇒ a single `Flag` either way.
///
/// Both parsers are total over their respective grammars: a parse
/// failure falls back to a single `Flag(stored)` (the unparsed
/// string). The comparator never panics on malformed input.
pub fn parse_stored_cfg(stored: &str) -> CfgAst {
    let s = stored.trim();
    if s.is_empty() {
        return CfgAst::Flag(String::new());
    }
    if looks_rust_functional(s)
        && let Some(ast) = parse_rust(s)
    {
        return ast;
    }
    if let Some(ast) = parse_go(s) {
        return ast;
    }
    // Defensive fallback: route through `CfgAst::flag` so cross-
    // language arch aliases still apply if both parsers reject the
    // input (rare — would mean malformed cfg storage).
    CfgAst::flag(s)
}

/// Cheap, conservative detection for the Rust-functional shape. The
/// `all(` / `any(` / `not(` substring tests are unambiguous (no Go
/// identifier reaches the open-paren without a binop in between);
/// `" = "` catches the `key = "value"` form.
fn looks_rust_functional(s: &str) -> bool {
    s.contains("all(") || s.contains("any(") || s.contains("not(") || s.contains(" = ")
}

// ---------------------------------------------------------------------------
// Platform-token normalisation
// ---------------------------------------------------------------------------

/// Rust `cfg` keys whose `key = "value"` form should be reduced to a
/// bare-value `Flag` so it matches Go's bare-token form. E.g.
/// `target_os = "linux"` → `Flag("linux")` matches Go's `Flag("linux")`.
///
/// Anything NOT in this set keeps its full `key=value` form to avoid
/// collapsing distinct meanings (`feature = "serde"` must NOT
/// collide with a bare `serde` flag elsewhere).
const PLATFORM_KEYS: &[&str] = &[
    "target_os",
    "target_arch",
    "target_family",
    "target_endian",
    "target_pointer_width",
    "target_env",
    "target_vendor",
    "target_abi",
];

/// Cross-language architecture token aliases. Maps each side to a
/// canonical form (the Go-side spelling — bare, lower-case) so a Rust
/// gate like `target_arch = "x86_64"` semantically equals a Go gate
/// like `//go:build amd64`. Without this, the platform-key reduction
/// at the parse site produces `Flag("x86_64")` on the Rust side and
/// `Flag("amd64")` on the Go side; structural equality fails. Codex
/// multi-LLM review iter-1 finding 1.
///
/// Pairs cover the Go GOARCH × Rust target_arch matrix where the
/// spellings differ. Tokens that match byte-for-byte across both
/// sides (e.g. `arm`, `mips`, `mips64`, `riscv64`, `s390x`,
/// `ppc64`, `ppc64le`) need no alias.
const ARCH_ALIASES: &[(&str, &str)] = &[
    ("x86_64", "amd64"),
    ("aarch64", "arm64"),
    ("x86", "386"),
    ("mips64el", "mips64le"),
    ("mipsel", "mipsle"),
    ("powerpc64", "ppc64"),
    ("powerpc64le", "ppc64le"),
    ("wasm32", "wasm"),
    ("wasm64", "wasm"),
];

/// Normalise a bare flag token. Translates known cross-language arch
/// aliases to their canonical (Go-side) spelling so Rust + Go gates
/// compare equal under `semantically_equals`. Anything not in the
/// alias table passes through unchanged so distinct identifiers
/// (e.g. `feature = "serde"` after platform-key reduction, or any
/// non-platform bare flag) keep their byte identity.
fn normalise_flag(s: &str) -> String {
    for (rust_form, canonical) in ARCH_ALIASES {
        if s == *rust_form {
            return (*canonical).to_string();
        }
    }
    s.to_string()
}

// ---------------------------------------------------------------------------
// Go-native parser  (subset of sqry-lang-go's build_constraints grammar)
// ---------------------------------------------------------------------------

fn parse_go(src: &str) -> Option<CfgAst> {
    let mut p = GoParser::new(src);
    let expr = p.parse_or()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return None;
    }
    Some(expr)
}

const MAX_DEPTH: usize = 128;

struct GoParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> GoParser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b' ' | b'\t') {
            self.pos += 1;
        }
    }
    fn enter(&mut self) -> Option<()> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        Some(())
    }
    fn leave(&mut self) {
        self.depth -= 1;
    }
    fn peek2(&self, a: u8, b: u8) -> bool {
        self.pos + 1 < self.bytes.len()
            && self.bytes[self.pos] == a
            && self.bytes[self.pos + 1] == b
    }
    fn parse_or(&mut self) -> Option<CfgAst> {
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
                    return None;
                }
                terms.push(self.parse_and()?);
            } else {
                break;
            }
        }
        self.leave();
        Some(if terms.len() == 1 {
            terms.into_iter().next().unwrap()
        } else {
            CfgAst::Any(terms)
        })
    }
    fn parse_and(&mut self) -> Option<CfgAst> {
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
                    return None;
                }
                terms.push(self.parse_unary()?);
            } else {
                break;
            }
        }
        self.leave();
        Some(if terms.len() == 1 {
            terms.into_iter().next().unwrap()
        } else {
            CfgAst::All(terms)
        })
    }
    fn parse_unary(&mut self) -> Option<CfgAst> {
        self.enter()?;
        self.skip_ws();
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'!' {
            self.pos += 1;
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                self.leave();
                return None;
            }
            let inner = self.parse_unary()?;
            self.leave();
            return Some(CfgAst::Not(Box::new(inner)));
        }
        let p = self.parse_primary()?;
        self.leave();
        Some(p)
    }
    fn parse_primary(&mut self) -> Option<CfgAst> {
        self.skip_ws();
        if self.pos >= self.bytes.len() {
            return None;
        }
        let c = self.bytes[self.pos];
        if c == b'(' {
            self.pos += 1;
            let inner = self.parse_or()?;
            self.skip_ws();
            if self.pos >= self.bytes.len() || self.bytes[self.pos] != b')' {
                return None;
            }
            self.pos += 1;
            return Some(inner);
        }
        if !is_ident_start(c) {
            return None;
        }
        let start = self.pos;
        while self.pos < self.bytes.len() && is_ident_cont(self.bytes[self.pos]) {
            self.pos += 1;
        }
        let ident = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        Some(CfgAst::Flag(normalise_flag(ident)))
    }
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}
fn is_ident_cont(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'.'
}

// ---------------------------------------------------------------------------
// Rust-functional parser  (subset of cfg_analysis grammar)
// ---------------------------------------------------------------------------
//
// Grammar:
//   expr := call | kv | ident
//   call := ('all'|'any'|'not') '(' args ')'
//   args := expr (',' expr)*
//   kv   := ident '=' '"' value '"'
//
// Platform-key kv (e.g. `target_os = "linux"`) lowers to `Flag(value)`;
// non-platform kv keeps the full `key=value` form.

fn parse_rust(src: &str) -> Option<CfgAst> {
    let mut p = RustParser::new(src);
    let expr = p.parse_expr()?;
    p.skip_ws();
    if p.pos != p.bytes.len() {
        return None;
    }
    Some(expr)
}

struct RustParser<'a> {
    bytes: &'a [u8],
    pos: usize,
    depth: usize,
}

impl<'a> RustParser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            bytes: s.as_bytes(),
            pos: 0,
            depth: 0,
        }
    }
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn at(&self, b: u8) -> bool {
        self.pos < self.bytes.len() && self.bytes[self.pos] == b
    }
    fn enter(&mut self) -> Option<()> {
        if self.depth >= MAX_DEPTH {
            return None;
        }
        self.depth += 1;
        Some(())
    }
    fn leave(&mut self) {
        self.depth -= 1;
    }
    fn parse_expr(&mut self) -> Option<CfgAst> {
        self.enter()?;
        self.skip_ws();
        let ident = self.take_ident()?;
        self.skip_ws();
        // Functional call: ident '(' args ')'
        if self.at(b'(') {
            self.pos += 1;
            let args = self.parse_args()?;
            self.skip_ws();
            if !self.at(b')') {
                self.leave();
                return None;
            }
            self.pos += 1;
            self.leave();
            return Some(match ident.as_str() {
                "all" => CfgAst::All(args),
                "any" => CfgAst::Any(args),
                "not" => {
                    if args.len() != 1 {
                        return None;
                    }
                    CfgAst::Not(Box::new(args.into_iter().next().unwrap()))
                }
                // Unknown functional name → preserve as Flag (defensive).
                _ => CfgAst::Flag(format!("{ident}(..)")),
            });
        }
        // kv form: ident '=' "value"
        if self.at(b'=') {
            self.pos += 1;
            self.skip_ws();
            if !self.at(b'"') {
                self.leave();
                return None;
            }
            self.pos += 1;
            let start = self.pos;
            while self.pos < self.bytes.len() && self.bytes[self.pos] != b'"' {
                self.pos += 1;
            }
            if !self.at(b'"') {
                self.leave();
                return None;
            }
            let value = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
            self.pos += 1;
            self.leave();
            // Normalise platform-key kv to bare-value Flag so cross-
            // language matching works. After reducing to the bare
            // value, apply `normalise_flag` so cross-language arch
            // aliases (target_arch = "x86_64" → amd64) collapse to
            // the canonical Go-side spelling. Codex multi-LLM iter-1
            // finding 1.
            let token = if PLATFORM_KEYS.contains(&ident.as_str()) {
                normalise_flag(value)
            } else {
                format!("{ident}={value}")
            };
            return Some(CfgAst::Flag(token));
        }
        // Bare ident.
        self.leave();
        Some(CfgAst::Flag(normalise_flag(&ident)))
    }
    fn parse_args(&mut self) -> Option<Vec<CfgAst>> {
        let mut out = Vec::new();
        self.skip_ws();
        if self.at(b')') {
            return Some(out);
        }
        loop {
            let e = self.parse_expr()?;
            out.push(e);
            self.skip_ws();
            if self.at(b',') {
                self.pos += 1;
                self.skip_ws();
            } else {
                break;
            }
        }
        Some(out)
    }
    fn take_ident(&mut self) -> Option<String> {
        let start = self.pos;
        if start >= self.bytes.len() || !is_ident_start(self.bytes[start]) {
            return None;
        }
        self.pos += 1;
        while self.pos < self.bytes.len() && is_ident_cont(self.bytes[self.pos]) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()
            .map(|s| s.to_string())
    }
}

// Set-equality + canonicalisation helpers live near `CfgAst::semantically_equals`
// above; `multiset_equals` was removed in iter-2 per 02_DESIGN §5.3.a.

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ----- Detection / parse dispatch ---------------------------------------

    #[test]
    fn parse_stored_cfg_detects_go_native() {
        assert_eq!(parse_stored_cfg("linux"), CfgAst::Flag("linux".into()));
        assert_eq!(
            parse_stored_cfg("linux && amd64"),
            CfgAst::All(vec![
                CfgAst::Flag("linux".into()),
                CfgAst::Flag("amd64".into())
            ])
        );
        assert_eq!(
            parse_stored_cfg("!windows"),
            CfgAst::Not(Box::new(CfgAst::Flag("windows".into())))
        );
        assert_eq!(
            parse_stored_cfg("linux || darwin"),
            CfgAst::Any(vec![
                CfgAst::Flag("linux".into()),
                CfgAst::Flag("darwin".into())
            ])
        );
    }

    #[test]
    fn parse_stored_cfg_detects_rust_functional() {
        assert_eq!(parse_stored_cfg("unix"), CfgAst::Flag("unix".into()));
        // target_os normalised to bare-value flag.
        assert_eq!(
            parse_stored_cfg("target_os = \"linux\""),
            CfgAst::Flag("linux".into())
        );
        // all(...) / any(...) / not(...) round-trip. Note that
        // `target_arch = "x86_64"` reduces via ARCH_ALIASES to the
        // canonical Go-side spelling `amd64` so cross-language
        // equality works (codex multi-LLM iter-1 finding 1).
        assert_eq!(
            parse_stored_cfg("all(unix, target_arch = \"x86_64\")"),
            CfgAst::All(vec![
                CfgAst::Flag("unix".into()),
                CfgAst::Flag("amd64".into())
            ])
        );
        assert_eq!(
            parse_stored_cfg("not(test)"),
            CfgAst::Not(Box::new(CfgAst::Flag("test".into())))
        );
        assert_eq!(
            parse_stored_cfg("any(unix, windows)"),
            CfgAst::Any(vec![
                CfgAst::Flag("unix".into()),
                CfgAst::Flag("windows".into())
            ])
        );
    }

    #[test]
    fn parse_stored_cfg_keeps_non_platform_kv_intact() {
        // `feature = "serde"` is NOT a platform key; preserve as
        // `feature=serde` so it doesn't collide with a bare `serde` flag.
        assert_eq!(
            parse_stored_cfg("feature = \"serde\""),
            CfgAst::Flag("feature=serde".into())
        );
    }

    // ----- Semantic equality ------------------------------------------------

    #[test]
    fn semantic_equals_normalises_target_os() {
        let go = parse_stored_cfg("linux");
        let rust = parse_stored_cfg("target_os = \"linux\"");
        assert!(go.semantically_equals(&rust));
        assert!(rust.semantically_equals(&go));
    }

    #[test]
    fn semantic_equals_normalises_target_arch_with_real_rust_spelling() {
        // Codex multi-LLM iter-1 finding 1: Rust gates use Rust's
        // own arch names — `target_arch = "x86_64"`, never
        // `target_arch = "amd64"`. Go uses GOARCH names — `amd64`.
        // Cross-language equality must apply the ARCH_ALIASES table.
        let go = parse_stored_cfg("amd64");
        let rust = parse_stored_cfg("target_arch = \"x86_64\"");
        assert!(
            go.semantically_equals(&rust),
            "Go `amd64` must match Rust `target_arch = \"x86_64\"` per ARCH_ALIASES",
        );
        assert!(
            rust.semantically_equals(&go),
            "symmetry: Rust x86_64 must match Go amd64",
        );

        // aarch64 ↔ arm64 — same shape, second pair.
        let go_arm = parse_stored_cfg("arm64");
        let rust_arm = parse_stored_cfg("target_arch = \"aarch64\"");
        assert!(
            go_arm.semantically_equals(&rust_arm),
            "Go `arm64` must match Rust `target_arch = \"aarch64\"`",
        );

        // 386 ↔ x86 — Go's 386 vs Rust's x86.
        let go_386 = parse_stored_cfg("386");
        let rust_x86 = parse_stored_cfg("target_arch = \"x86\"");
        assert!(
            go_386.semantically_equals(&rust_x86),
            "Go `386` must match Rust `target_arch = \"x86\"`",
        );
    }

    #[test]
    fn semantic_equals_cross_language_compound() {
        // Go: `linux && amd64`  vs Rust: `all(target_os = "linux",
        // target_arch = "x86_64")` (REAL Rust spelling — `x86_64`
        // not `amd64`). Per codex multi-LLM iter-1 finding 1.
        let go = parse_stored_cfg("linux && amd64");
        let rust = parse_stored_cfg("all(target_os = \"linux\", target_arch = \"x86_64\")");
        assert!(
            go.semantically_equals(&rust),
            "Go `linux && amd64` must match Rust `all(target_os = \"linux\", \
             target_arch = \"x86_64\")` after ARCH_ALIASES + platform-key reduction",
        );
    }

    #[test]
    fn canonical_flattens_nested_same_kind_compounds() {
        // Codex multi-LLM iter-1 finding 3: `all(linux, all(amd64,
        // cgo))` must canonicalise to `All([linux, amd64, cgo])`,
        // not `All([linux, All([amd64, cgo])])`. Without the flatten
        // step, semantically equivalent nested Rust cfg ASTs miss
        // their flat counterparts under struct_equals.
        let nested = CfgAst::All(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::All(vec![
                CfgAst::Flag("amd64".into()),
                CfgAst::Flag("cgo".into()),
            ]),
        ]);
        let flat = CfgAst::All(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("amd64".into()),
            CfgAst::Flag("cgo".into()),
        ]);
        assert!(
            nested.semantically_equals(&flat),
            "nested All-in-All must flatten under canonicalisation",
        );

        // Same for Any nesting.
        let nested_any = CfgAst::Any(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Any(vec![
                CfgAst::Flag("darwin".into()),
                CfgAst::Flag("freebsd".into()),
            ]),
        ]);
        let flat_any = CfgAst::Any(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("darwin".into()),
            CfgAst::Flag("freebsd".into()),
        ]);
        assert!(
            nested_any.semantically_equals(&flat_any),
            "nested Any-in-Any must flatten under canonicalisation",
        );

        // Cross-kind nesting (All-in-Any) must NOT flatten — semantic
        // precedence is preserved.
        let cross_kind = CfgAst::Any(vec![
            CfgAst::All(vec![
                CfgAst::Flag("linux".into()),
                CfgAst::Flag("amd64".into()),
            ]),
            CfgAst::Flag("darwin".into()),
        ]);
        let flat_wrong = CfgAst::Any(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("amd64".into()),
            CfgAst::Flag("darwin".into()),
        ]);
        assert!(
            !cross_kind.semantically_equals(&flat_wrong),
            "Any-of-All must NOT collapse — precedence is semantic",
        );
    }

    #[test]
    fn semantic_equals_set_order_invariant() {
        let a = parse_stored_cfg("linux && amd64");
        let b = parse_stored_cfg("amd64 && linux");
        assert!(a.semantically_equals(&b));
        // Same for Any.
        let c = parse_stored_cfg("linux || darwin");
        let d = parse_stored_cfg("darwin || linux");
        assert!(c.semantically_equals(&d));
    }

    #[test]
    fn semantic_equals_distinguishes_kinds() {
        let all = parse_stored_cfg("linux && amd64");
        let any = parse_stored_cfg("linux || amd64");
        assert!(!all.semantically_equals(&any));
    }

    #[test]
    fn semantic_equals_distinguishes_not() {
        let pos = parse_stored_cfg("windows");
        let neg = parse_stored_cfg("!windows");
        assert!(!pos.semantically_equals(&neg));
    }

    // ----- matches_stored ---------------------------------------------------

    #[test]
    fn literal_match_is_exact() {
        let m = CfgMatcher::Literal("linux && amd64".into());
        assert!(matches_stored(&m, "linux && amd64"));
        assert!(!matches_stored(&m, "amd64 && linux"));
        assert!(!matches_stored(&m, "linux"));
    }

    #[test]
    fn semantic_match_crosses_languages() {
        let q = parse_stored_cfg("linux");
        let m = CfgMatcher::Semantic(q);
        assert!(matches_stored(&m, "linux"));
        assert!(matches_stored(&m, "target_os = \"linux\""));
        assert!(matches_stored(&m, "linux && amd64"));
        assert!(matches_stored(
            &m,
            "all(target_os = \"linux\", target_arch = \"amd64\")"
        ));
        assert!(matches_stored(&m, "linux || darwin"));
        assert!(!matches_stored(&m, "darwin"));
        assert!(!matches_stored(&m, "target_os = \"darwin\""));
        assert!(!matches_stored(&m, "!linux"));
        assert!(!matches_stored(&m, "not(target_os = \"linux\")"));
    }

    #[test]
    fn semantic_match_compound_crosses_languages() {
        let q = parse_stored_cfg("linux && amd64");
        let m = CfgMatcher::Semantic(q);
        assert!(matches_stored(&m, "linux && amd64"));
        assert!(matches_stored(&m, "amd64 && linux"));
        assert!(matches_stored(
            &m,
            "all(target_os = \"linux\", target_arch = \"amd64\")"
        ));
        assert!(!matches_stored(&m, "linux"));
        assert!(!matches_stored(&m, "linux || amd64"));
    }

    // ----- Set-equality dedup (iter-2 fix) ---------------------------------

    #[test]
    fn semantic_equals_collapses_duplicate_operands_in_all() {
        // Cluster D's `conjoin` produces `All([linux, linux])` when
        // filename `*_linux.go` AND `//go:build linux` are both
        // present in the same source. The set-equality rule (per
        // 02_DESIGN §5.3.a) must collapse duplicates so that a bare
        // `cfg:linux` matcher still matches.
        let a = CfgAst::Flag("linux".into());
        let b = CfgAst::All(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("linux".into()),
        ]);
        assert!(a.semantically_equals(&b));
        assert!(b.semantically_equals(&a));
    }

    #[test]
    fn semantic_equals_collapses_duplicate_operands_in_any() {
        let a = CfgAst::Flag("linux".into());
        let b = CfgAst::Any(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("linux".into()),
        ]);
        assert!(a.semantically_equals(&b));
    }

    #[test]
    fn semantic_equals_dedups_then_set_equates() {
        // `All([linux, amd64, linux])` ≡ `All([amd64, linux])`
        let a = CfgAst::All(vec![
            CfgAst::Flag("linux".into()),
            CfgAst::Flag("amd64".into()),
            CfgAst::Flag("linux".into()),
        ]);
        let b = CfgAst::All(vec![
            CfgAst::Flag("amd64".into()),
            CfgAst::Flag("linux".into()),
        ]);
        assert!(a.semantically_equals(&b));
    }

    // ----- Robustness -------------------------------------------------------

    #[test]
    fn parse_stored_cfg_falls_back_to_flag_on_malformed_input() {
        // Truly malformed: unbalanced parens.
        let result = parse_stored_cfg("(((((");
        assert_eq!(result, CfgAst::Flag("(((((".into()));
    }

    #[test]
    fn parse_stored_cfg_empty_string_yields_empty_flag() {
        assert_eq!(parse_stored_cfg(""), CfgAst::Flag(String::new()));
        assert_eq!(parse_stored_cfg("   "), CfgAst::Flag(String::new()));
    }
}
