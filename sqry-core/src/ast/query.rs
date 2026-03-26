//! Query parser and evaluator for AST queries
//!
//! Implements the query language for searching code by AST structure.
//!
//! # Query Language
//!
//! The query language supports 7 predicate types:
//! - `kind:TYPE` - Match symbol kind
//! - `name~=REGEX` - Match name with regex
//! - `parent:TYPE` - Match parent kind
//! - `in:NAME` - Match if inside ancestor
//! - `depth:N` or `depth:>N` - Match nesting depth
//! - `path:TEXT` - Match symbol path
//! - `lang:LANG` - Match language
//!
//! Boolean operators: AND, OR, NOT with parentheses for grouping.
//!
//! # Example
//!
//! ```ignore
//! let query = parse_query("kind:function AND name~=^test_")?;
//! let matches: Vec<_> = contextual_matches.into_iter()
//!     .filter(|m| query.matches(m))
//!     .collect();
//! ```

use super::error::{AstQueryError, Result};
use super::types::{ContextKind, ContextualMatch};
use lru::LruCache;
use regex::Regex;
use std::num::NonZeroUsize;
use std::sync::Mutex;

/// Maximum allowed length for regex patterns (security limit)
const MAX_REGEX_LENGTH: usize = 1000;

/// Maximum value for repetition ranges (e.g., {n,m})
const MAX_REPETITION_COUNT: usize = 1000;

/// Validate regex pattern for security (prevent `DoS` via catastrophic backtracking)
fn validate_regex_pattern(pattern: &str) -> Result<()> {
    RegexSafetyScanner::new(pattern)?.scan()
}

struct RegexSafetyScanner<'a> {
    pattern: &'a str,
    chars: Vec<char>,
    pos: usize,
    state: RegexScanState,
}

impl<'a> RegexSafetyScanner<'a> {
    fn new(pattern: &'a str) -> Result<Self> {
        if pattern.len() > MAX_REGEX_LENGTH {
            return Err(invalid_regex(
                pattern,
                &format!(
                    "Regex pattern too long (max {} chars, got {})",
                    MAX_REGEX_LENGTH,
                    pattern.len()
                ),
            ));
        }

        Ok(Self {
            pattern,
            chars: pattern.chars().collect(),
            pos: 0,
            state: RegexScanState::default(),
        })
    }

    fn scan(mut self) -> Result<()> {
        while self.pos < self.chars.len() {
            let ch = self.chars[self.pos];

            if self.state.consume_escape(ch) {
                self.pos += 1;
                continue;
            }

            if self.state.update_bracket_state(ch) || self.state.in_brackets {
                self.pos += 1;
                continue;
            }

            // Check for alternation inside quantified groups: (a|b)+ or (a|b)*
            if self.is_quantified_group() {
                self.check_group_safety()?;
            }

            // Check for repetition ranges {n,m} and validate bounds
            if self.handle_repetition_range()? {
                continue;
            }

            // Check for simple nested repetition (a*+, a+*, a**, a++, etc.)
            if self.is_nested_repetition() {
                return Err(invalid_regex(
                    self.pattern,
                    "Regex pattern contains directly nested repetition operators",
                ));
            }

            self.pos += 1;
        }

        // Note: Consecutive quantifiers (e.g., .*.+, \w*\w*) are NOT validated.
        // Rust's regex crate is immune to catastrophic backtracking.
        Ok(())
    }

    fn is_quantified_group(&self) -> bool {
        if self.chars[self.pos] != ')' || self.pos + 1 >= self.chars.len() {
            return false;
        }
        matches!(self.chars[self.pos + 1], '+' | '*' | '?' | '{')
    }

    fn check_group_safety(&self) -> Result<()> {
        if self.has_alternation_in_group() {
            return Err(invalid_regex(
                self.pattern,
                "Regex pattern contains alternation inside quantified group like (a|b)+ which can cause exponential backtracking",
            ));
        }

        if self.has_quantifier_in_group() {
            return Err(invalid_regex(
                self.pattern,
                "Regex pattern contains nested quantifiers like (x+)+ or (x*)* which can cause catastrophic backtracking",
            ));
        }

        Ok(())
    }

    fn handle_repetition_range(&mut self) -> Result<bool> {
        if self.chars[self.pos] != '{' {
            return Ok(false);
        }

        if let Some(end_pos) = self.find_matching_brace() {
            let range_str: String = self.chars[self.pos + 1..end_pos].iter().collect();
            validate_repetition_range(&range_str, self.pattern)?;
            self.pos = end_pos + 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn is_nested_repetition(&self) -> bool {
        if self.pos + 1 >= self.chars.len() {
            return false;
        }
        let ch = self.chars[self.pos];
        let next_ch = self.chars[self.pos + 1];
        matches!(ch, '*' | '+' | '?') && matches!(next_ch, '*' | '+' | '?')
    }

    fn has_alternation_in_group(&self) -> bool {
        self.group_contains(false, |ch| ch == '|')
    }

    fn has_quantifier_in_group(&self) -> bool {
        self.group_contains(true, |ch| matches!(ch, '+' | '*' | '?'))
    }

    fn group_contains<F>(&self, escape_sensitive: bool, predicate: F) -> bool
    where
        F: Fn(char) -> bool,
    {
        let Some(start) = self.find_group_start(escape_sensitive) else {
            return false;
        };

        let scan_start = start + 1;
        if scan_start >= self.pos {
            return false;
        }

        self.chars[scan_start..self.pos]
            .iter()
            .copied()
            .any(predicate)
    }

    fn find_group_start(&self, escape_sensitive: bool) -> Option<usize> {
        let mut depth = 0;
        let mut i = self.pos;
        let mut escape_next = false;

        while i > 0 {
            i -= 1;
            let ch = self.chars[i];

            if escape_sensitive {
                if escape_next {
                    escape_next = false;
                    continue;
                }

                if ch == '\\' {
                    escape_next = true;
                    continue;
                }
            }

            match ch {
                ')' => depth += 1,
                '(' => {
                    if depth == 0 {
                        return Some(i);
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }

        None
    }

    fn find_matching_brace(&self) -> Option<usize> {
        for i in self.pos + 1..self.chars.len() {
            if self.chars[i] == '}' {
                return Some(i);
            }
            // Only digits and comma allowed in range
            if !self.chars[i].is_numeric() && self.chars[i] != ',' {
                return None;
            }
        }
        None
    }
}

#[derive(Default)]
struct RegexScanState {
    escape_next: bool,
    in_brackets: bool,
}

impl RegexScanState {
    fn consume_escape(&mut self, ch: char) -> bool {
        if self.escape_next {
            self.escape_next = false;
            return true;
        }
        if ch == '\\' {
            self.escape_next = true;
            return true;
        }
        false
    }

    fn update_bracket_state(&mut self, ch: char) -> bool {
        match ch {
            '[' => {
                self.in_brackets = true;
                true
            }
            ']' => {
                self.in_brackets = false;
                true
            }
            _ => false,
        }
    }
}

fn invalid_regex(pattern: &str, message: &str) -> AstQueryError {
    AstQueryError::InvalidRegex {
        pattern: pattern.to_string(),
        source: regex::Error::Syntax(message.to_string()),
    }
}

/// Validate repetition range values
fn validate_repetition_range(range_str: &str, pattern: &str) -> Result<()> {
    let parts: Vec<&str> = range_str.split(',').collect();

    for part in &parts {
        if part.is_empty() {
            continue;
        }

        match part.trim().parse::<usize>() {
            Ok(count) => {
                if count > MAX_REPETITION_COUNT {
                    return Err(AstQueryError::InvalidRegex {
                        pattern: pattern.to_string(),
                        source: regex::Error::Syntax(format!(
                            "Repetition count {count} exceeds maximum of {MAX_REPETITION_COUNT}"
                        )),
                    });
                }
            }
            Err(_) => {
                return Err(AstQueryError::InvalidRegex {
                    pattern: pattern.to_string(),
                    source: regex::Error::Syntax(format!("Invalid repetition range: {range_str}")),
                });
            }
        }
    }

    Ok(())
}

/// Depth comparison operator
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepthOp {
    /// Equal (depth:3)
    Eq(usize),
    /// Greater than (depth:>3)
    Gt(usize),
    /// Less than (depth:<3)
    Lt(usize),
    /// Greater than or equal (depth:>=3)
    Gte(usize),
    /// Less than or equal (depth:<=3)
    Lte(usize),
}

impl DepthOp {
    /// Check if a depth value matches this operator
    #[must_use]
    pub fn matches(&self, depth: usize) -> bool {
        match self {
            DepthOp::Eq(n) => depth == *n,
            DepthOp::Gt(n) => depth > *n,
            DepthOp::Lt(n) => depth < *n,
            DepthOp::Gte(n) => depth >= *n,
            DepthOp::Lte(n) => depth <= *n,
        }
    }
}

/// AST query predicate
#[derive(Debug, Clone)]
pub enum AstPredicate {
    /// Match symbol kind (kind:function)
    Kind(ContextKind),
    /// Match name with regex (name~=pattern)
    NameRegex(Regex),
    /// Match parent kind (parent:class)
    Parent(ContextKind),
    /// Match if inside ancestor (in:MyClass)
    In(String),
    /// Match nesting depth (depth:3 or depth:>3)
    Depth(DepthOp),
    /// Match symbol path (`path:module::function`)
    Path(String),
    /// Match language (lang:rust)
    Lang(String),
}

impl AstPredicate {
    /// Check if a contextual match satisfies this predicate
    #[must_use]
    pub fn matches(&self, ctx_match: &ContextualMatch) -> bool {
        match self {
            AstPredicate::Kind(kind) => ctx_match.context.kind == *kind,
            AstPredicate::NameRegex(regex) => regex.is_match(&ctx_match.name),
            AstPredicate::Parent(kind) => {
                if let Some(ref parent) = ctx_match.context.parent {
                    parent.kind == *kind
                } else {
                    false
                }
            }
            AstPredicate::In(name) => {
                // Check if any ancestor has this name
                ctx_match.context.ancestors.iter().any(|a| a.name == *name)
                    || ctx_match
                        .context
                        .parent
                        .as_ref()
                        .is_some_and(|p| p.name == *name)
            }
            AstPredicate::Depth(op) => op.matches(ctx_match.context.depth()),
            AstPredicate::Path(path) => ctx_match.context.path().contains(path),
            AstPredicate::Lang(lang) => ctx_match.language == *lang,
        }
    }
}

/// AST query expression (Boolean combination of predicates)
#[derive(Debug, Clone)]
pub enum AstExpr {
    /// Single predicate
    Predicate(AstPredicate),
    /// Logical AND
    And(Box<AstExpr>, Box<AstExpr>),
    /// Logical OR
    Or(Box<AstExpr>, Box<AstExpr>),
    /// Logical NOT
    Not(Box<AstExpr>),
}

impl AstExpr {
    /// Check if a contextual match satisfies this expression
    #[must_use]
    pub fn matches(&self, ctx_match: &ContextualMatch) -> bool {
        match self {
            AstExpr::Predicate(pred) => pred.matches(ctx_match),
            AstExpr::And(left, right) => left.matches(ctx_match) && right.matches(ctx_match),
            AstExpr::Or(left, right) => left.matches(ctx_match) || right.matches(ctx_match),
            AstExpr::Not(expr) => !expr.matches(ctx_match),
        }
    }
}

/// Maximum number of cached queries (LRU eviction beyond this)
const QUERY_CACHE_SIZE: usize = 100;

/// Global LRU cache for parsed queries
static QUERY_CACHE: std::sync::LazyLock<Mutex<LruCache<String, AstExpr>>> =
    std::sync::LazyLock::new(|| {
        Mutex::new(LruCache::new(NonZeroUsize::new(QUERY_CACHE_SIZE).unwrap()))
    });

/// Parse a query string into an AST expression
///
/// # Errors
///
/// Returns [`AstQueryError`] when the input contains unknown predicates,
/// invalid operators, malformed regex patterns, or invalid depth expressions.
pub fn parse_query(input: &str) -> Result<AstExpr> {
    // Check cache first (handle poisoned lock gracefully)
    {
        match QUERY_CACHE.lock() {
            Ok(mut cache) => {
                if let Some(cached) = cache.get(input) {
                    return Ok(cached.clone());
                }
            }
            Err(poisoned) => {
                let mut cache = poisoned.into_inner();
                if let Some(cached) = cache.get(input) {
                    return Ok(cached.clone());
                }
            }
        }
    }

    // Parse if not cached
    let mut parser = QueryParser::new(input);
    let expr = parser.parse_expr()?;

    // Cache the result (handle poisoned lock gracefully)
    {
        match QUERY_CACHE.lock() {
            Ok(mut cache) => {
                cache.put(input.to_string(), expr.clone());
            }
            Err(poisoned) => {
                let mut cache = poisoned.into_inner();
                cache.put(input.to_string(), expr.clone());
            }
        }
    }

    Ok(expr)
}

/// Clear the query cache (useful for testing)
#[cfg(test)]
#[allow(deprecated)]
pub fn clear_query_cache() {
    match QUERY_CACHE.lock() {
        Ok(mut cache) => cache.clear(),
        Err(poisoned) => poisoned.into_inner().clear(),
    }
}

/// Tokenizer for query strings
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Identifier(String),
    Colon,
    TildeEquals, // ~=
    LParen,
    RParen,
    And,
    Or,
    Not,
    Eof,
}

/// Query parser
struct QueryParser {
    chars: Vec<char>,
    pos: usize,
    current_token: Token,
}

impl QueryParser {
    fn new(input: &str) -> Self {
        let mut parser = Self {
            chars: input.chars().collect(),
            pos: 0,
            current_token: Token::Eof,
        };
        parser.current_token = parser.next_token();
        parser
    }

    fn parse_expr(&mut self) -> Result<AstExpr> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<AstExpr> {
        let mut left = self.parse_and()?;

        while matches!(self.current_token, Token::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = AstExpr::Or(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    fn parse_and(&mut self) -> Result<AstExpr> {
        let mut left = self.parse_not()?;

        while matches!(self.current_token, Token::And) {
            self.advance();
            let right = self.parse_not()?;
            left = AstExpr::And(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    fn parse_not(&mut self) -> Result<AstExpr> {
        if matches!(self.current_token, Token::Not) {
            self.advance();
            let expr = self.parse_not()?;
            Ok(AstExpr::Not(Box::new(expr)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<AstExpr> {
        if matches!(self.current_token, Token::LParen) {
            self.advance();
            let expr = self.parse_expr()?;
            if !matches!(self.current_token, Token::RParen) {
                return Err(AstQueryError::UnexpectedToken {
                    expected: ")".to_string(),
                    actual: format!("{:?}", self.current_token),
                });
            }
            self.advance();
            return Ok(expr);
        }

        self.parse_predicate()
    }

    fn parse_predicate(&mut self) -> Result<AstExpr> {
        let name = self.expect_identifier("predicate name")?;
        self.advance();

        let op = self.expect_operator()?;
        self.advance();

        let value = self.parse_predicate_value(&name, &op)?;

        Self::create_predicate(&name, op, value)
    }

    fn expect_identifier(&self, expected: &str) -> Result<String> {
        if let Token::Identifier(name) = &self.current_token {
            Ok(name.clone())
        } else {
            Err(AstQueryError::UnexpectedToken {
                expected: expected.to_string(),
                actual: format!("{:?}", self.current_token),
            })
        }
    }

    fn expect_operator(&self) -> Result<Token> {
        let op = self.current_token.clone();
        if !matches!(op, Token::Colon | Token::TildeEquals) {
            return Err(AstQueryError::UnexpectedToken {
                expected: ":' or '~='".to_string(),
                actual: format!("{op:?}"),
            });
        }
        Ok(op)
    }

    fn parse_predicate_value(&mut self, name: &str, op: &Token) -> Result<String> {
        if name == "name" && matches!(op, Token::TildeEquals) {
            self.read_regex_value()
        } else if let Token::Identifier(val) = &self.current_token {
            let v = val.clone();
            self.advance();
            Ok(v)
        } else {
            Err(AstQueryError::UnexpectedToken {
                expected: "value".to_string(),
                actual: format!("{:?}", self.current_token),
            })
        }
    }

    fn create_predicate(name: &str, op: Token, value: String) -> Result<AstExpr> {
        let predicate = match (name, op) {
            ("kind", Token::Colon) => AstPredicate::Kind(Self::parse_context_kind(&value)?),
            ("name", Token::TildeEquals) => {
                validate_regex_pattern(&value)?;
                let regex = Regex::new(&value).map_err(|e| AstQueryError::InvalidRegex {
                    pattern: value.clone(),
                    source: e,
                })?;
                AstPredicate::NameRegex(regex)
            }
            ("parent", Token::Colon) => AstPredicate::Parent(Self::parse_context_kind(&value)?),
            ("in", Token::Colon) => AstPredicate::In(value),
            ("depth", Token::Colon) => AstPredicate::Depth(Self::parse_depth_op(&value)?),
            ("path", Token::Colon) => AstPredicate::Path(value),
            ("lang", Token::Colon) => AstPredicate::Lang(value),
            (pred, _) => {
                return Err(AstQueryError::UnknownPredicate {
                    predicate: pred.to_string(),
                });
            }
        };

        Ok(AstExpr::Predicate(predicate))
    }

    fn parse_context_kind(s: &str) -> Result<ContextKind> {
        match s {
            "function" => Ok(ContextKind::Function),
            "method" => Ok(ContextKind::Method),
            "class" => Ok(ContextKind::Class),
            "struct" => Ok(ContextKind::Struct),
            "interface" => Ok(ContextKind::Interface),
            "enum" => Ok(ContextKind::Enum),
            "trait" => Ok(ContextKind::Trait),
            "module" => Ok(ContextKind::Module),
            "constant" => Ok(ContextKind::Constant),
            "variable" => Ok(ContextKind::Variable),
            "type" | "typealias" => Ok(ContextKind::TypeAlias),
            "impl" => Ok(ContextKind::Impl),
            _ => Err(AstQueryError::ParseError(format!(
                "Unknown context kind: {s}"
            ))),
        }
    }

    fn parse_depth_op(s: &str) -> Result<DepthOp> {
        if let Some(rest) = s.strip_prefix(">=") {
            Self::parse_depth_num(rest, DepthOp::Gte, s)
        } else if let Some(rest) = s.strip_prefix("<=") {
            Self::parse_depth_num(rest, DepthOp::Lte, s)
        } else if let Some(rest) = s.strip_prefix('>') {
            Self::parse_depth_num(rest, DepthOp::Gt, s)
        } else if let Some(rest) = s.strip_prefix('<') {
            Self::parse_depth_num(rest, DepthOp::Lt, s)
        } else {
            Self::parse_depth_num(s, DepthOp::Eq, s)
        }
    }

    fn parse_depth_num<F>(num_str: &str, ctor: F, original: &str) -> Result<DepthOp>
    where
        F: FnOnce(usize) -> DepthOp,
    {
        num_str
            .parse()
            .map(ctor)
            .map_err(|_| AstQueryError::InvalidDepth {
                value: original.to_string(),
            })
    }

    fn read_regex_value(&mut self) -> Result<String> {
        let mut value = String::new();
        let mut paren_depth = 0;

        loop {
            match &self.current_token {
                Token::Identifier(s) => {
                    value.push_str(s);
                    self.advance();
                }
                Token::LParen => {
                    value.push('(');
                    paren_depth += 1;
                    self.advance();
                }
                Token::RParen => {
                    if paren_depth == 0 {
                        break;
                    }
                    value.push(')');
                    paren_depth -= 1;
                    self.advance();
                }
                Token::And | Token::Or | Token::Eof => {
                    break;
                }
                _ => {
                    return Err(AstQueryError::UnexpectedToken {
                        expected: "regex value".to_string(),
                        actual: format!("{:?}", self.current_token),
                    });
                }
            }
        }

        if value.is_empty() {
            return Err(AstQueryError::UnexpectedToken {
                expected: "regex value".to_string(),
                actual: "empty".to_string(),
            });
        }

        Ok(value)
    }

    fn advance(&mut self) {
        self.current_token = self.next_token();
    }

    fn next_token(&mut self) -> Token {
        self.skip_whitespace();

        if self.pos >= self.chars.len() {
            return Token::Eof;
        }

        let ch = self.chars[self.pos];

        match ch {
            '(' => self.consume_char(Token::LParen),
            ')' => self.consume_char(Token::RParen),
            ':' => self.consume_char(Token::Colon),
            '~' => self.scan_tilde(),
            _ if Self::is_identifier_char(ch) => self.read_identifier(),
            _ => self.consume_char(Token::Identifier(ch.to_string())),
        }
    }

    fn consume_char(&mut self, token: Token) -> Token {
        self.pos += 1;
        token
    }

    fn scan_tilde(&mut self) -> Token {
        if self.peek_char(1) == Some('=') {
            self.pos += 2;
            Token::TildeEquals
        } else {
            self.pos += 1;
            Token::Identifier("~".to_string())
        }
    }

    fn is_identifier_char(ch: char) -> bool {
        ch.is_alphanumeric()
            || matches!(
                ch,
                '_' | '>'
                    | '<'
                    | '='
                    | '^'
                    | '$'
                    | '.'
                    | '/'
                    | '-'
                    | '*'
                    | '+'
                    | '?'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '|'
                    | '\\'
                    | ','
            )
    }

    fn read_identifier(&mut self) -> Token {
        let start = self.pos;
        while self.pos < self.chars.len() {
            let ch = self.chars[self.pos];
            if Self::is_identifier_char(ch) {
                self.pos += 1;
            } else {
                break;
            }
        }

        let text: String = self.chars[start..self.pos].iter().collect();
        match text.to_uppercase().as_str() {
            "AND" => Token::And,
            "OR" => Token::Or,
            "NOT" => Token::Not,
            _ => Token::Identifier(text),
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.current_char() {
            if ch.is_whitespace() {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn current_char(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_char(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_test_match(
        name: &str,
        kind: ContextKind,
        parent: Option<(&str, ContextKind)>,
        ancestors: Vec<(&str, ContextKind)>,
        lang: &str,
    ) -> ContextualMatch {
        let immediate =
            super::super::types::ContextItem::new(name.to_string(), kind, 1, 10, 0, 100);

        let parent_item = parent.map(|(pname, pkind)| {
            super::super::types::ContextItem::new(pname.to_string(), pkind, 1, 20, 0, 200)
        });

        let ancestor_items: Vec<_> = ancestors
            .into_iter()
            .map(|(aname, akind)| {
                super::super::types::ContextItem::new(aname.to_string(), akind, 1, 30, 0, 300)
            })
            .collect();

        let context = super::super::types::Context::new(immediate, parent_item, ancestor_items);

        let location = super::super::types::ContextualMatchLocation::new(
            PathBuf::from("test.rs"),
            1,
            0,
            10,
            1,
        );
        ContextualMatch::new(name.to_string(), location, context, lang.to_string())
    }

    #[test]
    fn test_parse_kind_predicate() {
        let query = parse_query("kind:function").unwrap();
        let test_match = make_test_match("test", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match));
    }

    #[test]
    fn test_parse_name_regex() {
        let query = parse_query("name~=test").unwrap();
        let test_match = make_test_match("test_func", ContextKind::Function, None, vec![], "rust");
        let other_match =
            make_test_match("other_func", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match), "Should match test_func");
        assert!(!query.matches(&other_match), "Should not match other_func");
    }

    #[test]
    fn test_parse_parent_predicate() {
        let query = parse_query("parent:class").unwrap();
        let test_match = make_test_match(
            "method",
            ContextKind::Method,
            Some(("MyClass", ContextKind::Class)),
            vec![],
            "rust",
        );
        let no_parent = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match));
        assert!(!query.matches(&no_parent));
    }

    #[test]
    fn test_parse_in_predicate() {
        let query = parse_query("in:MyClass").unwrap();
        let test_match = make_test_match(
            "method",
            ContextKind::Method,
            Some(("InnerClass", ContextKind::Class)),
            vec![("MyClass", ContextKind::Class)],
            "rust",
        );
        let not_in = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match));
        assert!(!query.matches(&not_in));
    }

    #[test]
    fn test_parse_depth_operators() {
        let eq_query = parse_query("depth:2").unwrap();
        let greater_than_query = parse_query("depth:>1").unwrap();
        let less_than_query = parse_query("depth:<3").unwrap();
        let greater_equal_query = parse_query("depth:>=2").unwrap();
        let less_equal_query = parse_query("depth:<=2").unwrap();

        let depth_2 = make_test_match(
            "method",
            ContextKind::Method,
            Some(("MyClass", ContextKind::Class)),
            vec![],
            "rust",
        );

        assert!(eq_query.matches(&depth_2));
        assert!(greater_than_query.matches(&depth_2));
        assert!(less_than_query.matches(&depth_2));
        assert!(greater_equal_query.matches(&depth_2));
        assert!(less_equal_query.matches(&depth_2));
    }

    #[test]
    fn test_parse_path_predicate() {
        let query = parse_query("path:MyClass").unwrap();
        let test_match = make_test_match(
            "method",
            ContextKind::Method,
            Some(("MyClass", ContextKind::Class)),
            vec![],
            "rust",
        );
        let no_match = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match));
        assert!(!query.matches(&no_match));
    }

    #[test]
    fn test_parse_lang_predicate() {
        let query = parse_query("lang:rust").unwrap();
        let rust_match = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        let js_match = make_test_match("func", ContextKind::Function, None, vec![], "javascript");
        assert!(query.matches(&rust_match));
        assert!(!query.matches(&js_match));
    }

    #[test]
    fn test_parse_and_expression() {
        let query = parse_query("kind:method AND parent:class").unwrap();
        let match_both = make_test_match(
            "method",
            ContextKind::Method,
            Some(("MyClass", ContextKind::Class)),
            vec![],
            "rust",
        );
        let match_kind_only = make_test_match("method", ContextKind::Method, None, vec![], "rust");
        assert!(query.matches(&match_both));
        assert!(!query.matches(&match_kind_only));
    }

    #[test]
    fn test_parse_or_expression() {
        let query = parse_query("kind:function OR kind:method").unwrap();
        let func_match = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        let method_match = make_test_match("method", ContextKind::Method, None, vec![], "rust");
        let class_match = make_test_match("MyClass", ContextKind::Class, None, vec![], "rust");
        assert!(query.matches(&func_match));
        assert!(query.matches(&method_match));
        assert!(!query.matches(&class_match));
    }

    #[test]
    fn test_parse_not_expression() {
        let query = parse_query("NOT kind:class").unwrap();
        let func_match = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        let class_match = make_test_match("MyClass", ContextKind::Class, None, vec![], "rust");
        assert!(query.matches(&func_match));
        assert!(!query.matches(&class_match));
    }

    #[test]
    fn test_parse_parentheses() {
        let query = parse_query("(kind:method AND parent:class) OR kind:function").unwrap();
        let method_in_class = make_test_match(
            "method",
            ContextKind::Method,
            Some(("MyClass", ContextKind::Class)),
            vec![],
            "rust",
        );
        let func = make_test_match("func", ContextKind::Function, None, vec![], "rust");
        let method_no_parent = make_test_match("method", ContextKind::Method, None, vec![], "rust");
        assert!(query.matches(&method_in_class));
        assert!(query.matches(&func));
        assert!(!query.matches(&method_no_parent));
    }

    #[test]
    fn test_parse_complex_query() {
        let query = parse_query("kind:method AND depth:>0 AND NOT in:TestClass").unwrap();
        let matching = make_test_match(
            "method",
            ContextKind::Method,
            Some(("MyClass", ContextKind::Class)),
            vec![],
            "rust",
        );
        let in_test_class = make_test_match(
            "method",
            ContextKind::Method,
            Some(("TestClass", ContextKind::Class)),
            vec![],
            "rust",
        );
        assert!(query.matches(&matching));
        assert!(!query.matches(&in_test_class));
    }

    #[test]
    fn test_parse_error_unknown_predicate() {
        let result = parse_query("unknown:value");
        assert!(matches!(
            result,
            Err(AstQueryError::UnknownPredicate { .. })
        ));
    }

    #[test]
    fn test_parse_error_invalid_regex() {
        let result = parse_query("name~=[invalid");
        assert!(matches!(result, Err(AstQueryError::InvalidRegex { .. })));
    }

    #[test]
    fn test_parse_error_invalid_depth() {
        let result = parse_query("depth:abc");
        assert!(matches!(result, Err(AstQueryError::InvalidDepth { .. })));
    }

    #[test]
    fn test_parse_error_missing_rparen() {
        let result = parse_query("(kind:function");
        assert!(matches!(result, Err(AstQueryError::UnexpectedToken { .. })));
    }

    #[test]
    fn test_depth_op_matches() {
        assert!(DepthOp::Eq(3).matches(3));
        assert!(!DepthOp::Eq(3).matches(2));

        assert!(DepthOp::Gt(2).matches(3));
        assert!(!DepthOp::Gt(2).matches(2));

        assert!(DepthOp::Lt(3).matches(2));
        assert!(!DepthOp::Lt(3).matches(3));

        assert!(DepthOp::Gte(2).matches(2));
        assert!(DepthOp::Gte(2).matches(3));
        assert!(!DepthOp::Gte(2).matches(1));

        assert!(DepthOp::Lte(3).matches(3));
        assert!(DepthOp::Lte(3).matches(2));
        assert!(!DepthOp::Lte(3).matches(4));
    }

    #[test]
    fn test_case_insensitive_keywords() {
        let query1 = parse_query("kind:function and name~=test").unwrap();
        let query2 = parse_query("kind:function AND name~=test").unwrap();
        let query3 = parse_query("kind:function AnD name~=test").unwrap();
        let test_match = make_test_match("test", ContextKind::Function, None, vec![], "rust");
        assert!(query1.matches(&test_match));
        assert!(query2.matches(&test_match));
        assert!(query3.matches(&test_match));
    }

    #[test]
    fn test_parse_unicode_identifier() {
        let unicode_identifier = "\u{6a21}\u{5757}\u{540d}";
        let query = parse_query(&format!("in:{unicode_identifier}")).unwrap();
        let test_match = make_test_match(
            "method",
            ContextKind::Method,
            Some((unicode_identifier, ContextKind::Class)),
            vec![],
            "rust",
        );
        assert!(query.matches(&test_match));
    }

    #[test]
    fn test_regex_validation_too_long() {
        let long_pattern = "a".repeat(1001);
        let query_str = format!("name~={long_pattern}");
        let result = parse_query(&query_str);
        assert!(matches!(result, Err(AstQueryError::InvalidRegex { .. })));
        if let Err(AstQueryError::InvalidRegex { pattern, .. }) = result {
            assert_eq!(pattern.len(), 1001);
        }
    }

    #[test]
    fn test_regex_validation_catastrophic_backtracking_nested_plus() {
        let result = validate_regex_pattern("(a+)+");
        assert!(result.is_err(), "Nested quantifiers should be rejected");
    }

    #[test]
    fn test_regex_validation_catastrophic_backtracking_nested_star() {
        let result = validate_regex_pattern("(a*)*");
        assert!(result.is_err(), "Nested quantifiers should be rejected");
    }

    #[test]
    fn test_regex_validation_nested_repetition_star_plus() {
        let result = parse_query("name~=a*+");
        assert!(matches!(result, Err(AstQueryError::InvalidRegex { .. })));
    }

    #[test]
    fn test_regex_validation_nested_repetition_plus_star() {
        let result = parse_query("name~=a+*");
        assert!(matches!(result, Err(AstQueryError::InvalidRegex { .. })));
    }

    #[test]
    fn test_regex_validation_safe_patterns_allowed() {
        let safe_patterns = vec![
            "name~=test",
            "name~=^test_.*",
            "name~=foo|bar",
            "name~=[a-z]+",
            "name~=\\w+",
            "name~=test{1,5}",
        ];
        for pattern in safe_patterns {
            let result = parse_query(pattern);
            assert!(result.is_ok(), "Safe pattern should be allowed: {pattern}");
        }
    }

    #[test]
    fn test_regex_validation_reasonable_length_allowed() {
        let pattern = "a".repeat(999);
        let query_str = format!("name~={pattern}");
        let result = parse_query(&query_str);
        assert!(result.is_ok(), "999-char pattern should be allowed");
    }

    #[test]
    fn test_validate_regex_pattern_directly() {
        assert!(validate_regex_pattern("test").is_ok());
        assert!(
            validate_regex_pattern("[a-z]+").is_ok(),
            "Character class with quantifier should be allowed"
        );
        assert!(
            validate_regex_pattern("(a+)+").is_err(),
            "Nested quantifiers should be rejected"
        );
        assert!(
            validate_regex_pattern("(a*)*").is_err(),
            "Nested quantifiers should be rejected"
        );
        assert!(
            validate_regex_pattern("a*+").is_err(),
            "Direct nested quantifiers should be rejected"
        );
    }

    #[test]
    fn test_regex_validation_alternation_explosion_simple() {
        let result = validate_regex_pattern("(a|ab)*");
        assert!(
            result.is_err(),
            "Alternation in quantified group should be rejected"
        );
    }

    #[test]
    fn test_regex_validation_alternation_explosion_plus() {
        let result = validate_regex_pattern("(foo|bar)+");
        assert!(result.is_err(), "Alternation with + should be rejected");
    }

    #[test]
    fn test_regex_validation_alternation_safe() {
        let result = validate_regex_pattern("(foo|bar)");
        assert!(
            result.is_ok(),
            "Alternation without quantifier should be allowed"
        );
    }

    #[test]
    fn test_regex_validation_alternation_in_character_class() {
        let result = validate_regex_pattern("[a|b]+");
        assert!(
            result.is_ok(),
            "Pipe inside character class should be allowed"
        );
    }

    #[test]
    fn test_regex_validation_large_repetition_range() {
        let result = validate_regex_pattern("a{1,999999}");
        assert!(result.is_err(), "Large repetition range should be rejected");
    }

    #[test]
    fn test_regex_validation_safe_repetition_range() {
        let result = validate_regex_pattern("a{1,5}");
        assert!(result.is_ok(), "Small repetition range should be allowed");
    }

    #[test]
    fn test_regex_validation_exact_max_repetition() {
        let result = validate_regex_pattern("a{1000}");
        assert!(result.is_ok(), "Repetition at max limit should be allowed");
    }

    #[test]
    fn test_regex_validation_just_over_max_repetition() {
        let result = validate_regex_pattern("a{1001}");
        assert!(
            result.is_err(),
            "Repetition over max limit should be rejected"
        );
    }

    #[test]
    fn test_regex_validation_open_ended_range() {
        let result = validate_regex_pattern("a{100,}");
        assert!(result.is_ok(), "Open-ended range should be allowed");
    }

    #[test]
    fn test_regex_validation_combined_attacks() {
        assert!(
            validate_regex_pattern("(a|ab){1,999999}").is_err(),
            "Combined alternation + large range should be rejected"
        );
        assert!(
            validate_regex_pattern("(a+|b+)*").is_err(),
            "Alternation with quantified branches in quantified group should be rejected"
        );
    }

    #[test]
    fn test_query_cache_basic() {
        clear_query_cache();
        let query1 = parse_query("kind:function").unwrap();
        let query2 = parse_query("kind:function").unwrap();
        let test_match = make_test_match("test", ContextKind::Function, None, vec![], "rust");
        assert!(query1.matches(&test_match));
        assert!(query2.matches(&test_match));
    }

    #[test]
    fn test_query_cache_different_queries() {
        clear_query_cache();
        let query1 = parse_query("kind:function").unwrap();
        let query2 = parse_query("kind:method").unwrap();
        let func_match = make_test_match("test", ContextKind::Function, None, vec![], "rust");
        let method_match = make_test_match("test", ContextKind::Method, None, vec![], "rust");
        assert!(query1.matches(&func_match));
        assert!(!query1.matches(&method_match));
        assert!(!query2.matches(&func_match));
        assert!(query2.matches(&method_match));
    }

    #[test]
    fn test_query_cache_eviction() {
        clear_query_cache();
        for i in 0..150 {
            let query_str = format!("kind:function AND name~=test{i}");
            let _ = parse_query(&query_str).unwrap();
        }
        for i in 0..150 {
            let query_str = format!("kind:function AND name~=test{i}");
            let result = parse_query(&query_str);
            assert!(result.is_ok(), "Query {i} should parse successfully");
        }
    }

    #[test]
    fn test_query_cache_with_errors() {
        clear_query_cache();
        let result1 = parse_query("invalid~syntax");
        assert!(result1.is_err());
        let result2 = parse_query("invalid~syntax");
        assert!(result2.is_err());
        let result3 = parse_query("kind:function");
        assert!(result3.is_ok());
    }

    #[test]
    fn test_query_cache_clear() {
        clear_query_cache();
        let _ = parse_query("kind:function").unwrap();
        clear_query_cache();
        let query = parse_query("kind:function").unwrap();
        let test_match = make_test_match("test", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match));
    }

    #[test]
    fn test_query_cache_thread_safety() {
        use std::thread;
        clear_query_cache();
        let handles: Vec<_> = (0..10)
            .map(|_| {
                thread::spawn(|| {
                    let query = parse_query("kind:function").unwrap();
                    let test_match =
                        make_test_match("test", ContextKind::Function, None, vec![], "rust");
                    assert!(query.matches(&test_match));
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
    }

    #[test]
    fn test_query_cache_complex_queries() {
        clear_query_cache();
        let complex_queries = vec![
            "kind:function AND name~=^test_",
            "(kind:method AND parent:class) OR kind:function",
            "depth:>3 AND NOT in:TestClass",
            "kind:function AND name~=.*helper.* AND depth:<=2",
        ];
        for query_str in &complex_queries {
            let query1 = parse_query(query_str).unwrap();
            let query2 = parse_query(query_str).unwrap();
            let test_match =
                make_test_match("test_func", ContextKind::Function, None, vec![], "rust");
            assert_eq!(query1.matches(&test_match), query2.matches(&test_match));
        }
    }

    #[test]
    fn test_query_cache_poison_recovery() {
        use std::sync::{Arc, Barrier};
        use std::thread;
        clear_query_cache();
        let _ = parse_query("kind:function").unwrap();
        let barrier = Arc::new(Barrier::new(5));
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    let query = parse_query("kind:function").unwrap();
                    let test_match =
                        make_test_match("test", ContextKind::Function, None, vec![], "rust");
                    assert!(query.matches(&test_match));
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let query = parse_query("kind:function").unwrap();
        let test_match = make_test_match("test", ContextKind::Function, None, vec![], "rust");
        assert!(query.matches(&test_match));
    }
}
