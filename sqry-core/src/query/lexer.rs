//! Lexer for the query language
//!
//! This module implements tokenization of query strings into a stream of tokens.
//! Supports keywords (AND, OR, NOT), operators (:, ~=, >, <, etc.), string literals,
//! regex literals with flags, numbers, and identifiers.

use crate::query::error::LexError;
use crate::query::types::{RegexFlags, Span};
use log::trace;
use std::cell::RefCell;
use std::env;
use std::str::Chars;
use std::thread_local;

#[cfg(all(test, feature = "dhat-heap"))]
#[global_allocator]
static DHAT_ALLOC: dhat::Alloc = dhat::Alloc;

/// A token type in the query language
#[derive(Debug, Clone, PartialEq)]
pub enum TokenType {
    // Keywords
    /// AND keyword
    And,
    /// OR keyword
    Or,
    /// NOT keyword
    Not,

    // Operators
    /// `:` operator (exact match / glob for paths)
    Colon,
    /// `~=` operator (regex match)
    RegexOp,
    /// `>` operator (greater than)
    Greater,
    /// `<` operator (less than)
    Less,
    /// `>=` operator (greater than or equal)
    GreaterEq,
    /// `<=` operator (less than or equal)
    LessEq,
    /// `|` pipe operator (for aggregation pipeline)
    Pipe,

    // Delimiters
    /// `(` left parenthesis
    LParen,
    /// `)` right parenthesis
    RParen,

    // Values
    /// Identifier (field names)
    Identifier(String),
    /// String literal (double or single quoted)
    StringLiteral(String),
    /// Regex literal with pattern and flags
    RegexLiteral {
        /// Regex pattern
        pattern: String,
        /// Regex flags (case-insensitive, multiline, dot-all)
        flags: RegexFlags,
    },
    /// Number literal
    NumberLiteral(i64),
    /// Boolean literal
    BooleanLiteral(bool),
    /// Bare word (unquoted value)
    Word(String),
    /// Variable reference (`$name`)
    Variable(String),

    // Special
    /// End of input
    Eof,
}

/// A token with position information
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The type of token
    pub token_type: TokenType,
    /// Source position
    pub span: Span,
}

impl Token {
    /// Create a new token
    #[must_use]
    pub fn new(token_type: TokenType, span: Span) -> Self {
        Self { token_type, span }
    }
}

/// Lexer state for tokenization
pub(crate) struct RawLexer<'a> {
    /// Input string
    input: &'a str,
    /// Character iterator
    chars: Chars<'a>,
    /// Current position (byte offset)
    position: usize,
    /// Current line (1-based)
    line: usize,
    /// Current column (1-based)
    column: usize,
    /// Peeked character (for lookahead)
    peeked: Option<char>,
}

impl<'a> RawLexer<'a> {
    /// Create a new lexer for the given input
    pub fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.chars(),
            position: 0,
            line: 1,
            column: 1,
            peeked: None,
        }
    }

    /// Reset lexer state to the beginning of the current input.
    pub fn restart(&mut self) {
        self.chars = self.input.chars();
        self.position = 0;
        self.line = 1;
        self.column = 1;
        self.peeked = None;
    }

    /// Tokenize the input, appending tokens to the provided buffer.
    pub fn tokenize_into(&mut self, tokens: &mut Vec<Token>) -> Result<(), LexError> {
        loop {
            let token = self.next_token()?;
            let is_eof = matches!(token.token_type, TokenType::Eof);
            tokens.push(token);

            if is_eof {
                break;
            }
        }

        Ok(())
    }

    /// Get the next token
    #[allow(clippy::too_many_lines)] // Lexer state machine is clearer when kept in one place.
    pub fn next_token(&mut self) -> Result<Token, LexError> {
        self.skip_whitespace();

        let start_pos = self.position;
        let start_line = self.line;
        let start_col = self.column;

        let Some(ch) = self.peek_char() else {
            return Ok(Token::new(
                TokenType::Eof,
                Span::with_position(self.position, self.position, self.line, self.column),
            ));
        };

        let token_type = if let Some(token) = self.read_simple_token(ch) {
            token
        } else if ch == '$' {
            self.read_variable_token(start_pos, start_line, start_col)?
        } else if ch == '~' {
            self.read_regex_operator(start_pos, start_line, start_col)?
        } else if ch == '>' || ch == '<' {
            self.read_comparison_operator(ch)
        } else if ch == '"' || ch == '\'' {
            let s = self.read_quoted_string(ch)?;
            TokenType::StringLiteral(s)
        } else if ch == '/' {
            let (pattern, flags) = self.read_regex()?;
            TokenType::RegexLiteral { pattern, flags }
        } else if self.is_number_start(ch) {
            let n = self.read_number()?;
            TokenType::NumberLiteral(n)
        } else if Self::is_word_start(ch) {
            self.read_word_token()
        } else {
            return Err(LexError::UnexpectedChar {
                char: ch,
                span: Span::with_position(
                    start_pos,
                    start_pos + ch.len_utf8(),
                    start_line,
                    start_col,
                ),
            });
        };

        Ok(Token::new(
            token_type,
            Span::with_position(start_pos, self.position, start_line, start_col),
        ))
    }

    fn read_simple_token(&mut self, ch: char) -> Option<TokenType> {
        let token = match ch {
            '(' => TokenType::LParen,
            ')' => TokenType::RParen,
            ':' => TokenType::Colon,
            '|' => TokenType::Pipe,
            _ => return None,
        };
        self.next_char();
        Some(token)
    }

    fn read_regex_operator(
        &mut self,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> Result<TokenType, LexError> {
        self.next_char();
        if self.peek_char() == Some('=') {
            self.next_char();
            Ok(TokenType::RegexOp)
        } else {
            Err(LexError::UnexpectedChar {
                char: '~',
                span: Span::with_position(start_pos, self.position, start_line, start_col),
            })
        }
    }

    fn read_comparison_operator(&mut self, ch: char) -> TokenType {
        self.next_char();
        let (equal, plain) = if ch == '>' {
            (TokenType::GreaterEq, TokenType::Greater)
        } else {
            (TokenType::LessEq, TokenType::Less)
        };
        if self.peek_char() == Some('=') {
            self.next_char();
            equal
        } else {
            plain
        }
    }

    fn is_number_start(&self, ch: char) -> bool {
        ch.is_ascii_digit() || (ch == '-' && self.peek_ahead(1).is_some_and(|c| c.is_ascii_digit()))
    }

    fn is_word_start(ch: char) -> bool {
        ch.is_ascii_alphabetic() || ch == '_'
    }

    /// Read a variable token: `$name` where name is alphanumeric/underscore.
    fn read_variable_token(
        &mut self,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> Result<TokenType, LexError> {
        self.next_char(); // consume '$'

        // Read the variable name (must be non-empty, alphanumeric + underscore)
        let mut name = String::new();
        while let Some(c) = self.peek_char() {
            if c.is_ascii_alphanumeric() || c == '_' {
                name.push(c);
                self.next_char();
            } else {
                break;
            }
        }

        if name.is_empty() {
            return Err(LexError::UnexpectedChar {
                char: '$',
                span: Span::with_position(start_pos, self.position, start_line, start_col),
            });
        }

        Ok(TokenType::Variable(name))
    }

    fn read_word_token(&mut self) -> TokenType {
        let word = self.read_word();
        match word.to_uppercase().as_str() {
            "AND" => TokenType::And,
            "OR" => TokenType::Or,
            "NOT" => TokenType::Not,
            "TRUE" => TokenType::BooleanLiteral(true),
            "FALSE" => TokenType::BooleanLiteral(false),
            _ => {
                self.skip_whitespace();
                match self.peek_char() {
                    Some(':' | '~' | '>' | '<') => TokenType::Identifier(word),
                    _ => TokenType::Word(word),
                }
            }
        }
    }

    /// Peek at the next character without consuming it
    fn peek_char(&mut self) -> Option<char> {
        if self.peeked.is_none() {
            self.peeked = self.chars.next();
        }
        self.peeked
    }

    /// Peek ahead n characters
    fn peek_ahead(&self, n: usize) -> Option<char> {
        self.input[self.position..].chars().nth(n)
    }

    /// Consume and return the next character
    fn next_char(&mut self) -> Option<char> {
        let ch = if let Some(c) = self.peeked.take() {
            Some(c)
        } else {
            self.chars.next()
        };

        if let Some(c) = ch {
            self.position += c.len_utf8();
            if c == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }

        ch
    }

    /// Skip whitespace characters
    fn skip_whitespace(&mut self) {
        while let Some(c) = self.peek_char() {
            if c.is_whitespace() {
                self.next_char();
            } else {
                break;
            }
        }
    }

    /// Read a quoted string with escape handling
    fn read_quoted_string(&mut self, quote: char) -> Result<String, LexError> {
        let start_pos = self.position;
        let start_line = self.line;
        let start_col = self.column;
        self.next_char(); // Skip opening quote

        let mut result = String::new();

        loop {
            match self.next_char() {
                Some(c) if c == quote => {
                    // Closing quote
                    return Ok(result);
                }
                Some('\\') => {
                    let escaped = self.read_escape_sequence(start_pos, start_line, start_col)?;
                    result.push(escaped);
                }
                Some(c) => result.push(c),
                None => {
                    return Err(LexError::UnterminatedString {
                        span: Span::with_position(start_pos, self.position, start_line, start_col),
                    });
                }
            }
        }
    }

    fn read_escape_sequence(
        &mut self,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> Result<char, LexError> {
        match self.next_char() {
            Some('"') => Ok('"'),
            Some('\'') => Ok('\''),
            Some('\\') => Ok('\\'),
            Some('n') => Ok('\n'),
            Some('t') => Ok('\t'),
            Some('r') => Ok('\r'),
            Some('u') => self.read_unicode_escape(),
            // Glob metacharacter escapes - passed through literally for glob pattern matching
            Some('*') => Ok('*'),
            Some('?') => Ok('?'),
            Some('[') => Ok('['),
            Some(']') => Ok(']'),
            Some('{') => Ok('{'),
            Some('}') => Ok('}'),
            Some(c) => Err(LexError::InvalidEscape {
                char: c,
                span: Span::with_position(self.position - 2, self.position, self.line, self.column),
            }),
            None => Err(LexError::UnterminatedString {
                span: Span::with_position(start_pos, self.position, start_line, start_col),
            }),
        }
    }

    fn read_unicode_escape(&mut self) -> Result<char, LexError> {
        // Unicode escape: \uXXXX
        let hex = self.read_hex_digits(4)?;
        let code_point =
            u32::from_str_radix(&hex, 16).map_err(|_| LexError::InvalidUnicodeEscape {
                got: hex.chars().next().unwrap_or('?'),
                span: Span::with_position(
                    self.position - hex.len() - 2,
                    self.position,
                    self.line,
                    self.column,
                ),
            })?;
        let ch = char::from_u32(code_point).ok_or_else(|| LexError::InvalidUnicodeEscape {
            got: hex.chars().next().unwrap_or('?'),
            span: Span::with_position(
                self.position - hex.len() - 2,
                self.position,
                self.line,
                self.column,
            ),
        })?;
        Ok(ch)
    }

    /// Read a regex literal: /pattern/flags
    fn read_regex(&mut self) -> Result<(String, RegexFlags), LexError> {
        let start_pos = self.position;
        let start_line = self.line;
        let start_col = self.column;
        self.next_char(); // Skip opening /

        let pattern = self.read_regex_pattern(start_pos, start_line, start_col)?;
        let flags = self.read_regex_flags(start_pos, start_line, start_col, &pattern)?;
        self.validate_regex_pattern(&pattern, &flags, start_pos, start_line, start_col)?;
        Ok((pattern, flags))
    }

    fn read_regex_pattern(
        &mut self,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> Result<String, LexError> {
        let mut pattern = String::new();

        // Read pattern until closing /
        loop {
            match self.next_char() {
                Some('/') => {
                    // Count trailing backslashes to determine if slash is escaped
                    let trailing_backslashes =
                        pattern.chars().rev().take_while(|&c| c == '\\').count();

                    if trailing_backslashes % 2 == 1 {
                        // Odd number of backslashes: last one escapes the slash
                        pattern.push('/');
                        continue;
                    }
                    // Even number (or zero): slash is not escaped, end of pattern
                    break;
                }
                Some(c) => pattern.push(c),
                None => {
                    return Err(LexError::UnterminatedRegex {
                        span: Span::with_position(start_pos, self.position, start_line, start_col),
                    });
                }
            }
        }

        Ok(pattern)
    }

    fn read_regex_flags(
        &mut self,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
        pattern: &str,
    ) -> Result<RegexFlags, LexError> {
        let mut flags = RegexFlags::default();
        while let Some(ch) = self.peek_char() {
            match ch {
                'i' => {
                    flags.case_insensitive = true;
                    self.next_char();
                }
                'm' => {
                    flags.multiline = true;
                    self.next_char();
                }
                's' => {
                    flags.dot_all = true;
                    self.next_char();
                }
                _ if ch.is_ascii_alphabetic() => {
                    // Unknown flag - return error
                    return Err(LexError::InvalidRegex {
                        pattern: pattern.to_string(),
                        error: format!("Unknown regex flag '{ch}'"),
                        span: Span::with_position(
                            start_pos,
                            self.position + 1,
                            start_line,
                            start_col,
                        ),
                    });
                }
                _ => break,
            }
        }

        Ok(flags)
    }

    fn validate_regex_pattern(
        &self,
        pattern: &str,
        flags: &RegexFlags,
        start_pos: usize,
        start_line: usize,
        start_col: usize,
    ) -> Result<(), LexError> {
        let mut builder = regex::RegexBuilder::new(pattern);
        builder
            .case_insensitive(flags.case_insensitive)
            .multi_line(flags.multiline)
            .dot_matches_new_line(flags.dot_all);

        if let Err(e) = builder.build() {
            return Err(LexError::InvalidRegex {
                pattern: pattern.to_string(),
                error: e.to_string(),
                span: Span::with_position(start_pos, self.position, start_line, start_col),
            });
        }

        Ok(())
    }

    /// Read hexadecimal digits for Unicode escapes
    fn read_hex_digits(&mut self, count: usize) -> Result<String, LexError> {
        let mut hex = String::new();

        for _ in 0..count {
            match self.next_char() {
                Some(c) if c.is_ascii_hexdigit() => hex.push(c),
                Some(c) => {
                    return Err(LexError::InvalidUnicodeEscape {
                        got: c,
                        span: Span::with_position(
                            self.position - 1,
                            self.position,
                            self.line,
                            self.column.saturating_sub(1),
                        ),
                    });
                }
                None => {
                    return Err(LexError::InvalidUnicodeEscape {
                        got: '?',
                        span: Span::with_position(
                            self.position,
                            self.position,
                            self.line,
                            self.column,
                        ),
                    });
                }
            }
        }

        Ok(hex)
    }

    /// Read a number (integer, possibly negative, possibly with underscores)
    fn read_number(&mut self) -> Result<i64, LexError> {
        let start_pos = self.position;
        let start_line = self.line;
        let start_col = self.column;
        let mut num_str = String::new();

        // Handle negative sign
        if self.peek_char() == Some('-') {
            num_str.push('-');
            self.next_char();
        }

        // Read digits (with optional underscores)
        while let Some(c) = self.peek_char() {
            if c.is_ascii_digit() {
                num_str.push(c);
                self.next_char();
            } else if c == '_' {
                // Skip underscores
                self.next_char();
            } else {
                break;
            }
        }

        // Parse the number
        num_str
            .parse::<i64>()
            .map_err(|e| LexError::NumberOverflow {
                text: num_str.clone(),
                error: e.to_string(),
                span: Span::with_position(start_pos, self.position, start_line, start_col),
            })
    }

    /// Read a word (identifier or keyword).
    /// Supports characters: [a-zA-Z0-9_.*?/-]+ plus generic segments like `<T,U>`.
    fn read_word(&mut self) -> String {
        let mut word = String::new();

        while let Some(c) = self.peek_char() {
            match self.classify_word_char(c) {
                WordCharType::Basic => {
                    word.push(c);
                    self.next_char();
                }
                WordCharType::DoubleColon => {
                    word.push_str("::");
                    self.next_char();
                    self.next_char();
                }
                WordCharType::GenericStart => {
                    self.consume_generic_segment(&mut word);
                }
                WordCharType::End => break,
            }
        }

        word
    }

    /// Classify a character for word parsing.
    fn classify_word_char(&self, c: char) -> WordCharType {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '*' | '?' | '/' | '-' | '[' | ']') {
            WordCharType::Basic
        } else if c == ':' && self.peek_ahead(1) == Some(':') {
            WordCharType::DoubleColon
        } else if c == '<' && self.has_generic_closing_angle() {
            WordCharType::GenericStart
        } else {
            WordCharType::End
        }
    }

    /// Consume a generic segment like `<T,U>` into the word buffer.
    fn consume_generic_segment(&mut self, word: &mut String) {
        word.push('<');
        self.next_char();

        let mut depth = 1usize;
        while let Some(ch) = self.peek_char() {
            if ch.is_whitespace() {
                break;
            }
            depth = match ch {
                '<' => depth.saturating_add(1),
                '>' => depth.saturating_sub(1),
                _ => depth,
            };
            word.push(ch);
            self.next_char();
            if depth == 0 {
                break;
            }
        }
    }

    /// Check if there's a matching closing angle bracket for generics.
    fn has_generic_closing_angle(&self) -> bool {
        let mut depth = 0usize;

        for ch in self.input[self.position..].chars() {
            if ch.is_whitespace() {
                return false;
            }
            match ch {
                '<' => depth = depth.saturating_add(1),
                '>' => {
                    if depth == 0 {
                        return false;
                    }
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }

        false
    }
}

/// Classification of characters for word parsing.
enum WordCharType {
    /// Basic word character (alphanumeric or special symbols).
    Basic,
    /// Double colon `::` for namespace separation.
    DoubleColon,
    /// Start of a generic segment `<`.
    GenericStart,
    /// End of word (not a valid word character).
    End,
}

/// Public lexer wrapper retained for compatibility until pooling integration lands.
pub struct Lexer<'a> {
    raw: RawLexer<'a>,
}

impl<'a> Lexer<'a> {
    /// Create a new lexer for the given input.
    #[must_use]
    pub fn new(input: &'a str) -> Self {
        Self {
            raw: RawLexer::new(input),
        }
    }

    /// Tokenize the entire input into a vector of tokens.
    ///
    /// # Errors
    ///
    /// Returns [`LexError`] when lexical analysis fails (unterminated strings, invalid regexes, etc.).
    pub fn tokenize(&mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::with_capacity(16);
        self.raw.restart();
        self.raw.tokenize_into(&mut tokens)?;
        Ok(tokens)
    }

    /// Fetch the next token from the stream (used in parser tests).
    ///
    /// # Errors
    ///
    /// Returns [`LexError`] when the next token cannot be produced.
    pub fn next_token(&mut self) -> Result<Token, LexError> {
        self.raw.next_token()
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShrinkPolicy {
    pub max_capacity: usize,
    pub shrink_ratio: usize,
}

impl Default for ShrinkPolicy {
    fn default() -> Self {
        Self {
            max_capacity: 256,
            shrink_ratio: 8,
        }
    }
}

// Runtime knobs (env-first) controlling the lexer pool.
const POOL_MAX_DEFAULT: usize = 4;
const ENV_POOL_MAX: &str = "SQRY_LEXER_POOL_MAX";
const ENV_POOL_MAX_CAP: &str = "SQRY_LEXER_POOL_MAX_CAP";
const ENV_POOL_SHRINK_RATIO: &str = "SQRY_LEXER_POOL_SHRINK_RATIO";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PoolConfig {
    max_size: usize,
    shrink_policy: ShrinkPolicy,
}

impl PoolConfig {
    fn default() -> Self {
        Self {
            max_size: POOL_MAX_DEFAULT,
            shrink_policy: ShrinkPolicy::default(),
        }
    }

    fn from_environment() -> Self {
        let mut config = Self::default();

        if let Some(value) = read_env_usize(ENV_POOL_MAX) {
            config.max_size = value;
        }

        if let Some(value) = read_env_usize(ENV_POOL_MAX_CAP) {
            config.shrink_policy.max_capacity = value.max(1);
        }

        if let Some(value) = read_env_usize(ENV_POOL_SHRINK_RATIO) {
            config.shrink_policy.shrink_ratio = value.max(1);
        }

        config
    }
}

fn read_env_usize(var: &str) -> Option<usize> {
    match env::var(var) {
        Ok(value) => match value.parse::<usize>() {
            Ok(parsed) => Some(parsed),
            Err(err) => {
                trace!("Ignoring invalid value for {var}: {err}");
                None
            }
        },
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            trace!("Ignoring non-unicode value for {var}");
            None
        }
    }
}

thread_local! {
    static LEXER_POOL: RefCell<LexerPool> = RefCell::new(LexerPool::new(PoolConfig::default()));
}

struct LexerPool {
    stash: Vec<ReusableLexer>,
    in_flight: usize,
    config: PoolConfig,
}

impl LexerPool {
    fn new(config: PoolConfig) -> Self {
        Self {
            stash: Vec::new(),
            in_flight: 0,
            config,
        }
    }

    fn apply_config(&mut self, config: PoolConfig) {
        if self.config == config {
            return;
        }

        trace!(
            "sqry::query::lexer: updating pool config -> max_size={}, max_capacity={}, shrink_ratio={}",
            config.max_size, config.shrink_policy.max_capacity, config.shrink_policy.shrink_ratio
        );

        self.config = config;
        self.stash.clear();
        self.in_flight = 0;
    }

    fn acquire(&mut self) -> LexerHandle {
        if let Some(lexer) = self.stash.pop() {
            self.in_flight += 1;
            return LexerHandle::pooled(lexer);
        }

        if self.in_flight < self.config.max_size {
            self.in_flight += 1;
            let lexer = ReusableLexer::with_policy(self.config.shrink_policy);
            return LexerHandle::pooled(lexer);
        }

        LexerHandle::temporary(ReusableLexer::with_policy(self.config.shrink_policy))
    }

    fn release(&mut self, lexer: ReusableLexer) {
        if self.config.max_size == 0 {
            self.in_flight = self.in_flight.saturating_sub(1);
            return;
        }

        self.in_flight = self.in_flight.saturating_sub(1);
        if self.stash.len() < self.config.max_size {
            self.stash.push(lexer);
        }
    }

    #[cfg(test)]
    fn stats(&self) -> (usize, usize, PoolConfig) {
        (self.stash.len(), self.in_flight, self.config)
    }

    #[cfg(test)]
    fn reset(&mut self, config: PoolConfig) {
        self.stash.clear();
        self.in_flight = 0;
        self.config = config;
    }
}

struct LexerHandle {
    lexer: Option<ReusableLexer>,
    pooled: bool,
}

impl LexerHandle {
    fn pooled(lexer: ReusableLexer) -> Self {
        Self {
            lexer: Some(lexer),
            pooled: true,
        }
    }

    fn temporary(lexer: ReusableLexer) -> Self {
        Self {
            lexer: Some(lexer),
            pooled: false,
        }
    }

    fn lexer_mut(&mut self) -> &mut ReusableLexer {
        // SAFETY: self.lexer is always Some during LexerHandle's lifetime.
        // It only becomes None during Drop (after moving to pool), which occurs
        // after all user operations complete. This expect() cannot panic during normal use.
        self.lexer.as_mut().expect("lexer handle missing lexer")
    }

    fn reset(&mut self, input: &str) {
        self.lexer_mut().reset(input);
    }

    fn tokenize(&mut self) -> Result<TokenBatch<'_>, LexError> {
        self.lexer_mut().tokenize()
    }
}

impl Drop for LexerHandle {
    fn drop(&mut self) {
        if !self.pooled {
            return;
        }

        if let Some(lexer) = self.lexer.take() {
            LEXER_POOL.with(|cell| {
                cell.borrow_mut().release(lexer);
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn configure_pool_for_tests(max_size: usize, shrink_policy: ShrinkPolicy) {
    LEXER_POOL.with(|cell| {
        cell.borrow_mut().reset(PoolConfig {
            max_size,
            shrink_policy,
        });
    });
}

#[cfg(test)]
pub(crate) fn reset_pool_to_default_for_tests() {
    configure_pool_for_tests(POOL_MAX_DEFAULT, ShrinkPolicy::default());
}

#[cfg(test)]
pub(crate) fn pool_stats_for_tests() -> (usize, usize, usize) {
    LEXER_POOL.with(|cell| {
        let (stash, in_flight, config) = cell.borrow().stats();
        (stash, in_flight, config.max_size)
    })
}

pub(crate) fn with_lexer<F, T>(input: &str, f: F) -> Result<T, LexError>
where
    F: FnOnce(TokenBatch<'_>) -> Result<T, LexError>,
{
    let config = PoolConfig::from_environment();

    if config.max_size == 0 {
        LEXER_POOL.with(|cell| {
            cell.borrow_mut().apply_config(config);
        });
        let mut lexer = ReusableLexer::with_policy(config.shrink_policy);
        lexer.reset(input);
        let batch = lexer.tokenize()?;
        return f(batch);
    }

    let mut handle = LEXER_POOL.with(|cell| {
        let mut pool = cell.borrow_mut();
        pool.apply_config(config);
        pool.acquire()
    });

    handle.reset(input);
    let batch = handle.tokenize()?;
    let result = f(batch);
    drop(handle);
    result
}

/// Tokenize using the thread-local lexer pool, returning owned tokens.
///
/// This is useful for benches and integration points that only need the token
/// stream and do not want to work with the internal `TokenBatch` guard.
///
/// # Errors
///
/// Returns [`LexError`] when lexical analysis fails.
pub fn tokenize_with_pool(input: &str) -> Result<Vec<Token>, LexError> {
    with_lexer(input, |batch| Ok(batch.into_vec()))
}

#[cfg(debug_assertions)]
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Copy)]
struct LexerDiagnostics {
    reuse_count: usize,
    max_capacity_seen: usize,
    shrink_count: usize,
}

#[cfg(debug_assertions)]
#[allow(dead_code)]
impl LexerDiagnostics {
    fn record_reuse(&mut self, capacity: usize) {
        self.reuse_count += 1;
        if capacity > self.max_capacity_seen {
            self.max_capacity_seen = capacity;
        }
    }

    fn record_shrink(&mut self) {
        self.shrink_count += 1;
    }
}

/// Policy controlling how aggressively reusable lexer buffers shrink.
/// Reusable lexer that owns its input, token buffer, and shrink policy.
#[allow(dead_code)]
pub(crate) struct ReusableLexer {
    input: String,
    token_buffer: Vec<Token>,
    shrink_policy: ShrinkPolicy,
    #[cfg(debug_assertions)]
    diagnostics: LexerDiagnostics,
}

#[allow(dead_code)]
impl ReusableLexer {
    pub fn new() -> Self {
        Self::with_policy(ShrinkPolicy::default())
    }

    pub fn with_policy(shrink_policy: ShrinkPolicy) -> Self {
        Self {
            input: String::new(),
            token_buffer: Vec::with_capacity(16),
            shrink_policy,
            #[cfg(debug_assertions)]
            diagnostics: LexerDiagnostics::default(),
        }
    }

    /// Reset the lexer to a new input string.
    pub fn reset(&mut self, input: &str) {
        self.input.clear();
        self.input.push_str(input);
        self.token_buffer.clear();
    }

    /// Tokenize the current input, returning an RAII guard over the buffer.
    pub fn tokenize(&mut self) -> Result<TokenBatch<'_>, LexError> {
        self.token_buffer.clear();
        let mut raw = RawLexer::new(self.input.as_str());
        raw.tokenize_into(&mut self.token_buffer)?;
        #[cfg(debug_assertions)]
        self.diagnostics.record_reuse(self.token_buffer.capacity());
        Ok(TokenBatch {
            tokens: &mut self.token_buffer,
            shrink_policy: self.shrink_policy,
            #[cfg(debug_assertions)]
            diagnostics: &mut self.diagnostics,
        })
    }

    #[cfg(debug_assertions)]
    fn diagnostics(&self) -> &LexerDiagnostics {
        &self.diagnostics
    }
}

/// RAII guard for the reusable token buffer.
///
/// Provides read-only access via `as_slice()` or transfers ownership via
/// `into_vec()`, draining the reusable buffer without cloning. The guard holds
/// a mutable borrow to the underlying buffer so additional tokenization cannot
/// start until it is dropped. On drop the buffer is cleared and, if the shrink
/// policy deems it oversized, the capacity is reduced.
#[allow(dead_code)]
pub(crate) struct TokenBatch<'a> {
    tokens: &'a mut Vec<Token>,
    shrink_policy: ShrinkPolicy,
    #[cfg(debug_assertions)]
    diagnostics: &'a mut LexerDiagnostics,
}

#[allow(dead_code)]
impl TokenBatch<'_> {
    pub fn as_slice(&self) -> &[Token] {
        self.tokens.as_slice()
    }

    #[allow(unused_mut)]
    pub fn into_vec(mut self) -> Vec<Token> {
        let result = self.tokens.drain(..).collect();
        #[cfg(debug_assertions)]
        let _ = &mut *self.diagnostics; // keep diagnostics reference alive for Drop
        result
    }
}

impl Drop for TokenBatch<'_> {
    fn drop(&mut self) {
        if !self.tokens.is_empty() {
            self.tokens.clear();
        }

        let shrink_threshold = self
            .shrink_policy
            .max_capacity
            .saturating_mul(self.shrink_policy.shrink_ratio);
        if shrink_threshold > 0 && self.tokens.capacity() > shrink_threshold {
            self.tokens.shrink_to(self.shrink_policy.max_capacity);
            #[cfg(debug_assertions)]
            self.diagnostics.record_shrink();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::{Mutex, OnceLock};

    #[cfg(feature = "dhat-heap")]
    use dhat::{HeapStats, Profiler};

    fn reset_pool_from_env() {
        let config = PoolConfig::from_environment();
        LEXER_POOL.with(|cell| {
            cell.borrow_mut().reset(config);
        });
    }

    fn reset_pool_default() {
        unsafe {
            std::env::remove_var(ENV_POOL_MAX);
            std::env::remove_var(ENV_POOL_MAX_CAP);
            std::env::remove_var(ENV_POOL_SHRINK_RATIO);
        }
        reset_pool_from_env();
    }

    fn set_env(var: &str, value: &str) {
        unsafe {
            std::env::set_var(var, value);
        }
    }

    fn remove_env(var: &str) {
        unsafe {
            std::env::remove_var(var);
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn reusable_lexer_reuses_buffer_across_calls() {
        let mut lexer = ReusableLexer::new();
        lexer.reset("kind:function");

        let first_ptr = {
            let batch = lexer.tokenize().unwrap();
            let ptr = batch.as_slice().as_ptr();
            assert!(!batch.as_slice().is_empty());
            ptr
        };
        assert_eq!(first_ptr, lexer.token_buffer.as_ptr());

        lexer.reset("name:test");
        let second_ptr = {
            let batch = lexer.tokenize().unwrap();
            let ptr = batch.as_slice().as_ptr();
            assert!(!batch.as_slice().is_empty());
            ptr
        };
        assert_eq!(second_ptr, lexer.token_buffer.as_ptr());
        assert_eq!(first_ptr, second_ptr);
        #[cfg(debug_assertions)]
        {
            let diagnostics = lexer.diagnostics();
            assert!(diagnostics.reuse_count >= 2);
            assert!(diagnostics.max_capacity_seen >= lexer.token_buffer.capacity());
        }
    }

    #[test]
    fn reusable_lexer_clears_buffer_on_panic() {
        let mut lexer = ReusableLexer::new();
        lexer.reset("kind:function");

        let result = catch_unwind(AssertUnwindSafe(|| {
            let _batch = lexer.tokenize().unwrap();
            panic!("boom");
        }));

        assert!(result.is_err());
        assert_eq!(lexer.token_buffer.len(), 0);
    }

    #[test]
    fn reusable_lexer_into_vec_drains_tokens() {
        let mut lexer = ReusableLexer::new();
        lexer.reset("kind:function");

        let tokens = {
            let batch = lexer.tokenize().unwrap();
            batch.into_vec()
        };

        assert_eq!(tokens.len(), 4);
        assert_eq!(lexer.token_buffer.len(), 0);
    }

    #[test]
    fn reusable_lexer_shrink_policy_applies() {
        let policy = ShrinkPolicy {
            max_capacity: 8,
            shrink_ratio: 2,
        };

        let mut lexer = ReusableLexer::with_policy(policy);
        let large_query = (0..128)
            .map(|i| format!("name:value{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        lexer.reset(&large_query);

        {
            let batch = lexer.tokenize().unwrap();
            let _ = batch.into_vec();
        }

        if lexer.token_buffer.capacity() <= policy.max_capacity * policy.shrink_ratio {
            lexer
                .token_buffer
                .reserve(policy.max_capacity * policy.shrink_ratio * 2);
        }
        assert!(lexer.token_buffer.capacity() > policy.max_capacity * policy.shrink_ratio);

        lexer.reset("kind:function");
        {
            let batch = lexer.tokenize().unwrap();
            drop(batch);
        }

        assert!(lexer.token_buffer.capacity() <= policy.max_capacity);

        #[cfg(debug_assertions)]
        {
            let diagnostics = lexer.diagnostics();
            assert!(diagnostics.shrink_count >= 1);
        }
    }

    #[test]
    fn lexer_pool_returns_lexers_to_stash() {
        let _guard = env_lock().lock().unwrap();
        reset_pool_default();

        assert_eq!(PoolConfig::from_environment().max_size, POOL_MAX_DEFAULT);

        let tokens = with_lexer("kind:function", |batch| Ok(batch.into_vec())).unwrap();
        assert_eq!(tokens.len(), 4);

        LEXER_POOL.with(|cell| {
            let (stash_len, in_flight, config) = cell.borrow().stats();
            assert_eq!(config.max_size, POOL_MAX_DEFAULT);
            assert_eq!(in_flight, 0);
            assert_eq!(stash_len, 1);
        });
    }

    #[test]
    fn lexer_pool_respects_zero_capacity_env() {
        let _guard = env_lock().lock().unwrap();
        set_env(ENV_POOL_MAX, "0");
        reset_pool_from_env();

        let tokens = with_lexer("kind:function", |batch| Ok(batch.into_vec())).unwrap();
        assert_eq!(tokens.len(), 4);

        LEXER_POOL.with(|cell| {
            let (stash_len, in_flight, config) = cell.borrow().stats();
            assert_eq!(config.max_size, 0);
            assert_eq!(in_flight, 0);
            assert_eq!(stash_len, 0);
        });

        remove_env(ENV_POOL_MAX);
        reset_pool_default();
    }

    #[test]
    fn lexer_pool_reuses_single_slot() {
        let _guard = env_lock().lock().unwrap();
        set_env(ENV_POOL_MAX, "1");
        reset_pool_from_env();

        assert_eq!(PoolConfig::from_environment().max_size, 1);

        for query in ["kind:function", "name:test"] {
            let _ = with_lexer(query, |batch| Ok(batch.into_vec())).unwrap();
        }

        LEXER_POOL.with(|cell| {
            let (stash_len, in_flight, config) = cell.borrow().stats();
            assert_eq!(config.max_size, 1);
            assert_eq!(in_flight, 0);
            assert_eq!(stash_len, 1);
        });

        remove_env(ENV_POOL_MAX);
        reset_pool_default();
    }

    #[test]
    fn lexer_handles_double_colon_in_words() {
        let mut lexer = Lexer::new("callers:Player::takeDamage");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 4); // Identifier, Colon, Word, Eof
        assert_eq!(
            tokens[0].token_type,
            TokenType::Identifier("callers".to_string())
        );
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        assert_eq!(
            tokens[2].token_type,
            TokenType::Word("Player::takeDamage".to_string())
        );
        assert!(matches!(tokens[3].token_type, TokenType::Eof));
    }

    #[test]
    #[ignore = "Test depends on clean env_lock state. Run in isolation with: cargo test -p sqry-core --lib with_lexer_allows_reentrant_usage -- --ignored --test-threads=1"]
    fn with_lexer_allows_reentrant_usage() {
        let _guard = env_lock().lock().unwrap();
        reset_pool_default();

        let result = with_lexer("kind:function", |batch| {
            assert!(!batch.as_slice().is_empty());
            with_lexer("name:test", |inner_batch| {
                assert!(!inner_batch.as_slice().is_empty());
                Ok(())
            })
        });

        assert!(result.is_ok());
        reset_pool_default();
    }

    #[test]
    fn lexer_pool_thread_local_isolation() {
        let _guard = env_lock().lock().unwrap();
        reset_pool_default();

        let handles: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    for _ in 0..50 {
                        for query in ["kind:function", "name:test", "lang:rust"] {
                            with_lexer(query, |batch| {
                                assert!(!batch.as_slice().is_empty());
                                Ok(batch.into_vec())
                            })
                            .unwrap();
                        }
                    }

                    let (stash, in_flight, max_size) = crate::query::lexer::pool_stats_for_tests();
                    assert!(stash <= max_size);
                    assert_eq!(in_flight, 0);
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap();
        }

        reset_pool_default();
    }

    #[cfg(feature = "dhat-heap")]
    #[test]
    #[ignore = "Heap profiling test must run in isolation. Run with: cargo test -p sqry-core --lib lexer_reuse_minimizes_heap_allocations -- --ignored --test-threads=1"]
    fn lexer_reuse_minimizes_heap_allocations() {
        let _guard = env_lock().lock().unwrap();
        reset_pool_default();

        let profiler = Profiler::new_heap();

        for _ in 0..5 {
            with_lexer("kind:function", |batch| Ok(batch.into_vec())).unwrap();
        }

        let stats = HeapStats::get();
        drop(profiler);
        // Threshold adjusted for integration test environment: when running with --tests,
        // all plugin crates (dev-dependencies) are loaded, increasing baseline allocations
        // from ~28 blocks (unit tests only) to ~58 blocks (with plugins loaded)
        assert!(
            stats.total_blocks <= 65,
            "expected limited allocations, observed {} blocks (threshold accounts for plugin loading in integration tests)",
            stats.total_blocks
        );

        reset_pool_default();
    }

    #[test]
    fn reusable_lexer_capacity_growth_and_retention() {
        let mut lexer = ReusableLexer::new();

        lexer.reset("kind:function");
        {
            let batch = lexer.tokenize().unwrap();
            assert!(!batch.as_slice().is_empty());
        }
        let initial_capacity = lexer.token_buffer.capacity();

        let large_query = (0..50)
            .map(|i| format!("name:value{i}"))
            .collect::<Vec<_>>()
            .join(" AND ");
        lexer.reset(&large_query);
        {
            let batch = lexer.tokenize().unwrap();
            assert!(batch.as_slice().len() > 50);
        }
        let grown_capacity = lexer.token_buffer.capacity();
        assert!(grown_capacity > initial_capacity);

        lexer.reset("kind:function");
        {
            let batch = lexer.tokenize().unwrap();
            assert!(!batch.as_slice().is_empty());
        }
        let retained_capacity = lexer.token_buffer.capacity();
        assert_eq!(retained_capacity, grown_capacity);

        #[cfg(debug_assertions)]
        {
            let diagnostics = lexer.diagnostics();
            assert!(diagnostics.reuse_count >= 3);
            assert!(diagnostics.max_capacity_seen >= grown_capacity);
        }
    }

    #[test]
    fn reusable_lexer_error_recovery_clears_buffer() {
        let mut lexer = ReusableLexer::new();

        lexer.reset("kind:function");
        {
            let batch = lexer.tokenize().unwrap();
            assert!(!batch.as_slice().is_empty());
        }

        lexer.reset("kind@invalid");
        let result = lexer.tokenize();
        assert!(result.is_err());
        drop(result);

        lexer.reset("name:test");
        {
            let batch = lexer.tokenize().unwrap();
            assert!(!batch.as_slice().is_empty());
        }
    }

    #[test]
    fn reusable_lexer_panic_after_into_vec_has_clean_buffer() {
        let mut lexer = ReusableLexer::new();
        lexer.reset("kind:function");

        let result = catch_unwind(AssertUnwindSafe(|| {
            let batch = lexer.tokenize().unwrap();
            let _tokens = batch.into_vec();
            panic!("boom");
        }));

        assert!(result.is_err());
        assert_eq!(lexer.token_buffer.len(), 0);
    }

    #[test]
    fn test_tokenize_simple_query() {
        let mut lexer = Lexer::new("kind:function");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens.len(), 4);
        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "kind"));
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        assert!(matches!(tokens[2].token_type, TokenType::Word(ref s) if s == "function"));
        assert!(matches!(tokens[3].token_type, TokenType::Eof));
    }

    #[test]
    fn test_tokenize_generic_type_value() {
        let mut lexer = Lexer::new("returns:Optional<User>");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "returns"));
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        assert!(matches!(tokens[2].token_type, TokenType::Word(ref s) if s == "Optional<User>"));
        assert!(matches!(tokens[3].token_type, TokenType::Eof));
    }

    #[test]
    fn test_tokenize_nested_generic_value() {
        let mut lexer = Lexer::new("returns:Map<String,List<Order>>");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "returns"));
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        assert!(
            matches!(tokens[2].token_type, TokenType::Word(ref s) if s == "Map<String,List<Order>>")
        );
        assert!(matches!(tokens[3].token_type, TokenType::Eof));
    }

    #[test]
    fn test_tokenize_numeric_comparison_after_identifier() {
        let mut lexer = Lexer::new("line>10");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "line"));
        assert!(matches!(tokens[1].token_type, TokenType::Greater));
        assert!(matches!(tokens[2].token_type, TokenType::NumberLiteral(10)));
        assert!(matches!(tokens[3].token_type, TokenType::Eof));
    }

    #[test]
    fn test_tokenize_keywords_case_insensitive() {
        let mut lexer = Lexer::new("AND and Or NOT not");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::And));
        assert!(matches!(tokens[1].token_type, TokenType::And));
        assert!(matches!(tokens[2].token_type, TokenType::Or));
        assert!(matches!(tokens[3].token_type, TokenType::Not));
        assert!(matches!(tokens[4].token_type, TokenType::Not));
    }

    #[test]
    fn test_tokenize_operators() {
        let mut lexer = Lexer::new(": ~= > < >= <=");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Colon));
        assert!(matches!(tokens[1].token_type, TokenType::RegexOp));
        assert!(matches!(tokens[2].token_type, TokenType::Greater));
        assert!(matches!(tokens[3].token_type, TokenType::Less));
        assert!(matches!(tokens[4].token_type, TokenType::GreaterEq));
        assert!(matches!(tokens[5].token_type, TokenType::LessEq));
    }

    #[test]
    fn test_tokenize_parentheses() {
        let mut lexer = Lexer::new("( )");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::LParen));
        assert!(matches!(tokens[1].token_type, TokenType::RParen));
    }

    #[test]
    fn test_tokenize_double_quoted_string() {
        let mut lexer = Lexer::new(r#"name:"hello world""#);
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "name"));
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        assert!(
            matches!(tokens[2].token_type, TokenType::StringLiteral(ref s) if s == "hello world")
        );
    }

    #[test]
    fn test_tokenize_single_quoted_string() {
        let mut lexer = Lexer::new(r"name:'hello world'");
        let tokens = lexer.tokenize().unwrap();

        assert!(
            matches!(tokens[2].token_type, TokenType::StringLiteral(ref s) if s == "hello world")
        );
    }

    #[test]
    fn test_string_escape_sequences() {
        let mut lexer = Lexer::new(r#""line1\nline2\ttab\"quote\\backslash""#);
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::StringLiteral(s) = &tokens[0].token_type {
            assert_eq!(s, "line1\nline2\ttab\"quote\\backslash");
        } else {
            panic!("Expected string literal");
        }
    }

    #[test]
    fn test_unicode_escape() {
        let mut lexer = Lexer::new(r#""\u0041BC""#);
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::StringLiteral(s) = &tokens[0].token_type {
            assert_eq!(s, "ABC");
        } else {
            panic!("Expected string literal");
        }
    }

    #[test]
    fn test_unterminated_string() {
        let mut lexer = Lexer::new(r#"name:"unclosed"#);
        let result = lexer.tokenize();
        assert!(matches!(result, Err(LexError::UnterminatedString { .. })));
    }

    #[test]
    fn test_invalid_escape() {
        let mut lexer = Lexer::new(r#""\x""#);
        let result = lexer.tokenize();
        assert!(matches!(
            result,
            Err(LexError::InvalidEscape { char: 'x', .. })
        ));
    }

    #[test]
    fn test_glob_metacharacter_escape_sequences() {
        // Test that glob metacharacters can be escaped in quoted strings
        // This is required for path: predicates with literal glob chars
        let mut lexer = Lexer::new(r#""src/\[test\]/\*\?file\{a,b\}""#);
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::StringLiteral(s) = &tokens[0].token_type {
            assert_eq!(s, "src/[test]/*?file{a,b}");
        } else {
            panic!("Expected string literal, got {:?}", tokens[0].token_type);
        }
    }

    #[test]
    fn test_path_predicate_with_escaped_glob_chars() {
        // Simulate a path: predicate with escaped glob characters
        let mut lexer = Lexer::new(r#"path:"src/\[test\]/**""#);
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "path"));
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        if let TokenType::StringLiteral(s) = &tokens[2].token_type {
            // The escaped brackets become literal brackets, ** remains as-is
            assert_eq!(s, "src/[test]/**");
        } else {
            panic!("Expected string literal");
        }
    }

    #[test]
    fn test_tokenize_regex() {
        let mut lexer = Lexer::new(r"name~=/^test_/i");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[0].token_type, TokenType::Identifier(ref s) if s == "name"));
        assert!(matches!(tokens[1].token_type, TokenType::RegexOp));

        if let TokenType::RegexLiteral { pattern, flags } = &tokens[2].token_type {
            assert_eq!(pattern, "^test_");
            assert!(flags.case_insensitive);
            assert!(!flags.multiline);
            assert!(!flags.dot_all);
        } else {
            panic!("Expected regex literal");
        }
    }

    #[test]
    fn test_regex_multiple_flags() {
        let mut lexer = Lexer::new(r"/pattern/ims");
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::RegexLiteral { flags, .. } = &tokens[0].token_type {
            assert!(flags.case_insensitive);
            assert!(flags.multiline);
            assert!(flags.dot_all);
        } else {
            panic!("Expected regex literal");
        }
    }

    #[test]
    fn test_regex_escaped_slash() {
        let mut lexer = Lexer::new(r"/path\/to\/file/");
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::RegexLiteral { pattern, .. } = &tokens[0].token_type {
            assert_eq!(pattern, r"path\/to\/file");
        } else {
            panic!("Expected regex literal");
        }
    }

    #[test]
    fn test_regex_escaped_backslash_then_slash() {
        // Input raw string r"/a\\\\/": This contains /a\\\\/
        // In the raw string, \\\\ is 4 literal backslash characters
        // So pattern before / is: a\\\\
        // Trailing backslashes: 4 (even number)
        // Even count means slash is NOT escaped, so pattern ends
        // Result pattern should be: a\\\\ (4 backslashes)
        let mut lexer = Lexer::new(r"/a\\\\/");
        let token = lexer.next_token().unwrap();
        match token.token_type {
            TokenType::RegexLiteral { pattern, .. } => {
                assert_eq!(pattern, r"a\\\\"); // 4 backslashes
            }
            _ => panic!("Expected RegexLiteral"),
        }
    }

    #[test]
    fn test_regex_single_escaped_slash() {
        let mut lexer = Lexer::new(r"/a\/b/"); // Pattern: a/b with escaped slash
        let token = lexer.next_token().unwrap();
        match token.token_type {
            TokenType::RegexLiteral { pattern, .. } => {
                assert_eq!(pattern, r"a\/b");
            }
            _ => panic!("Expected RegexLiteral"),
        }
    }

    #[test]
    fn test_unterminated_regex() {
        let mut lexer = Lexer::new(r"/unclosed");
        let result = lexer.tokenize();
        assert!(matches!(result, Err(LexError::UnterminatedRegex { .. })));
    }

    #[test]
    fn test_invalid_regex_pattern() {
        let mut lexer = Lexer::new(r"/^[/");
        let result = lexer.tokenize();
        assert!(matches!(result, Err(LexError::InvalidRegex { .. })));
    }

    #[test]
    fn test_regex_unknown_flag() {
        let mut lexer = Lexer::new("/pattern/x");
        let err = lexer.next_token().unwrap_err();
        match err {
            LexError::InvalidRegex { error, .. } => {
                assert!(error.contains("Unknown regex flag"));
            }
            _ => panic!("Expected InvalidRegex error"),
        }
    }

    #[test]
    fn test_tokenize_positive_number() {
        let mut lexer = Lexer::new("lines:42");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[2].token_type, TokenType::NumberLiteral(42)));
    }

    #[test]
    fn test_tokenize_negative_number() {
        let mut lexer = Lexer::new("lines:-42");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(
            tokens[2].token_type,
            TokenType::NumberLiteral(-42)
        ));
    }

    #[test]
    fn test_tokenize_number_with_underscores() {
        let mut lexer = Lexer::new("lines:1_000_000");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(
            tokens[2].token_type,
            TokenType::NumberLiteral(1_000_000)
        ));
    }

    #[test]
    fn test_number_overflow() {
        let mut lexer = Lexer::new("lines:99999999999999999999");
        let result = lexer.tokenize();
        assert!(matches!(result, Err(LexError::NumberOverflow { .. })));
    }

    #[test]
    fn test_tokenize_boolean_true() {
        let mut lexer = Lexer::new("async:true");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(
            tokens[2].token_type,
            TokenType::BooleanLiteral(true)
        ));
    }

    #[test]
    fn test_tokenize_boolean_false() {
        let mut lexer = Lexer::new("async:FALSE");
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(
            tokens[2].token_type,
            TokenType::BooleanLiteral(false)
        ));
    }

    #[test]
    fn test_tokenize_complex_query() {
        let mut lexer = Lexer::new(r"kind:function AND async:true OR name~=/^test_/i");
        let tokens = lexer.tokenize().unwrap();

        // Verify token sequence
        assert!(matches!(tokens[0].token_type, TokenType::Identifier(_)));
        assert!(matches!(tokens[1].token_type, TokenType::Colon));
        assert!(matches!(tokens[2].token_type, TokenType::Word(_)));
        assert!(matches!(tokens[3].token_type, TokenType::And));
        assert!(matches!(tokens[4].token_type, TokenType::Identifier(_)));
        assert!(matches!(tokens[5].token_type, TokenType::Colon));
        assert!(matches!(
            tokens[6].token_type,
            TokenType::BooleanLiteral(true)
        ));
        assert!(matches!(tokens[7].token_type, TokenType::Or));
        assert!(matches!(tokens[8].token_type, TokenType::Identifier(_)));
        assert!(matches!(tokens[9].token_type, TokenType::RegexOp));
        assert!(matches!(
            tokens[10].token_type,
            TokenType::RegexLiteral { .. }
        ));
        assert!(matches!(tokens[11].token_type, TokenType::Eof));
    }

    #[test]
    fn test_whitespace_handling() {
        let mut lexer = Lexer::new("  kind  :  function  ");
        let tokens = lexer.tokenize().unwrap();

        assert_eq!(tokens.len(), 4); // kind, :, function, EOF
    }

    #[test]
    fn test_unexpected_character() {
        let mut lexer = Lexer::new("kind@function");
        let result = lexer.tokenize();
        assert!(matches!(
            result,
            Err(LexError::UnexpectedChar { char: '@', .. })
        ));
    }

    #[test]
    fn test_empty_string_literal() {
        let mut lexer = Lexer::new(r#"name:"""#);
        let tokens = lexer.tokenize().unwrap();

        assert!(matches!(tokens[2].token_type, TokenType::StringLiteral(ref s) if s.is_empty()));
    }

    #[test]
    fn test_empty_regex_literal() {
        let mut lexer = Lexer::new(r"name~=//");
        let tokens = lexer.tokenize().unwrap();

        if let TokenType::RegexLiteral { pattern, .. } = &tokens[2].token_type {
            assert_eq!(pattern, "");
        } else {
            panic!("Expected regex literal");
        }
    }

    #[test]
    fn test_span_tracking() {
        let mut lexer = Lexer::new("kind:function");
        let tokens = lexer.tokenize().unwrap();

        // Verify spans are set
        assert!(tokens[0].span.start == 0);
        assert!(tokens[0].span.end == 4); // "kind"
        assert!(tokens[1].span.start == 4);
        assert!(tokens[1].span.end == 5); // ":"
        assert!(tokens[2].span.start == 5);
        assert!(tokens[2].span.end == 13); // "function"
    }

    #[test]
    fn test_identifier_vs_word() {
        let mut lexer = Lexer::new("kind:value value");
        let tokens = lexer.tokenize().unwrap();

        // First 'value' after ':' should be a Word
        assert!(matches!(tokens[2].token_type, TokenType::Word(ref s) if s == "value"));
        // Second 'value' standalone should also be a Word
        assert!(matches!(tokens[3].token_type, TokenType::Word(ref s) if s == "value"));
    }

    #[test]
    fn test_bare_word_with_glob() {
        let mut lexer = Lexer::new("path:src/*.rs");
        lexer.next_token().unwrap(); // path (identifier)
        lexer.next_token().unwrap(); // :
        let token = lexer.next_token().unwrap();
        match token.token_type {
            TokenType::Word(s) => assert_eq!(s, "src/*.rs"),
            _ => panic!(
                "Expected Word with glob pattern, got {:?}",
                token.token_type
            ),
        }
    }

    #[test]
    fn test_bare_word_with_hyphen() {
        let mut lexer = Lexer::new("name:foo-bar");
        lexer.next_token().unwrap(); // name
        lexer.next_token().unwrap(); // :
        let token = lexer.next_token().unwrap();
        match token.token_type {
            TokenType::Word(s) => assert_eq!(s, "foo-bar"),
            _ => panic!("Expected Word with hyphen, got {:?}", token.token_type),
        }
    }

    #[test]
    fn test_bare_word_with_dot() {
        let mut lexer = Lexer::new("path:foo.rs");
        lexer.next_token().unwrap(); // path
        lexer.next_token().unwrap(); // :
        let token = lexer.next_token().unwrap();
        match token.token_type {
            TokenType::Word(s) => assert_eq!(s, "foo.rs"),
            _ => panic!("Expected Word with dot, got {:?}", token.token_type),
        }
    }

    #[test]
    fn test_variable_token() {
        let mut lexer = Lexer::new("$name");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 2); // Variable, Eof
        assert_eq!(
            tokens[0].token_type,
            TokenType::Variable("name".to_string())
        );
        assert!(matches!(tokens[1].token_type, TokenType::Eof));
    }

    #[test]
    fn test_variable_token_with_underscores() {
        let mut lexer = Lexer::new("$my_var");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 2); // Variable, Eof
        assert_eq!(
            tokens[0].token_type,
            TokenType::Variable("my_var".to_string())
        );
    }

    #[test]
    fn test_pipe_token() {
        let mut lexer = Lexer::new("|");
        let tokens = lexer.tokenize().unwrap();
        assert_eq!(tokens.len(), 2); // Pipe, Eof
        assert!(matches!(tokens[0].token_type, TokenType::Pipe));
    }

    #[test]
    fn test_dollar_sign_alone_error() {
        let mut lexer = Lexer::new("$ ");
        let result = lexer.tokenize();
        assert!(
            matches!(result, Err(LexError::UnexpectedChar { char: '$', .. })),
            "Bare '$' should produce an error, got: {result:?}"
        );
    }
}
