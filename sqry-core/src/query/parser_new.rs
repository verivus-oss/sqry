//! Parser for the query language
//!
//! This module implements a recursive descent parser that builds an Abstract Syntax Tree (AST)
//! from a stream of tokens. The parser respects operator precedence: NOT > AND > OR.

use crate::config::buffers::{max_predicates, max_query_length};
use crate::query::error::ParseError;
use crate::query::lexer::{Token, TokenType};
use crate::query::types::{
    Condition, Expr, Field, JoinEdgeKind, JoinExpr, Operator, PipelineQuery, PipelineStage, Query,
    RegexFlags, RegexValue, Span, Value,
};

/// Parser for query language
pub struct Parser {
    /// Token stream
    tokens: Vec<Token>,
    /// Current position in token stream
    position: usize,
}

impl Parser {
    /// Create a new parser from a token stream
    #[must_use]
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            position: 0,
        }
    }

    /// Parse a query string into a Query AST
    ///
    /// This is a convenience method that lexes and parses in one call.
    /// Enforces `SQRY_MAX_QUERY_LENGTH` and `SQRY_MAX_PREDICATES` limits.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`](crate::query::error::QueryError) when lexing or parsing fails.
    pub fn parse_query(input: &str) -> Result<Query, crate::query::error::QueryError> {
        use crate::query::lexer::with_lexer;

        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ParseError::EmptyQuery.into());
        }

        let max_len = max_query_length();
        let input_len = input.len();
        if input_len > max_len {
            return Err(ParseError::InvalidSyntax {
                message: format!(
                    "Query too long: {input_len} bytes exceeds {max_len} byte limit. \
                     Adjust SQRY_MAX_QUERY_LENGTH environment variable if needed."
                ),
                span: Span::new(0, 0),
            }
            .into());
        }

        let tokens = with_lexer(input, |batch| Ok(batch.into_vec()))?;

        let mut parser = Parser::new(tokens);
        let query = parser.parse()?;

        let max_preds = max_predicates();
        let predicate_count = count_conditions(&query.root);
        if predicate_count > max_preds {
            return Err(ParseError::InvalidSyntax {
                message: format!(
                    "Too many predicates: {predicate_count} exceeds {max_preds} limit. \
                     Adjust SQRY_MAX_PREDICATES environment variable if needed."
                ),
                span: Span::new(0, 0),
            }
            .into());
        }

        Ok(query)
    }

    /// Parse a query string into a `PipelineQuery` if it contains pipe operators,
    /// or return `None` if no pipes are present (use `parse_query` instead).
    ///
    /// # Errors
    ///
    /// Returns [`QueryError`](crate::query::error::QueryError) when lexing or parsing fails.
    pub fn parse_pipeline_query(
        input: &str,
    ) -> Result<Option<PipelineQuery>, crate::query::error::QueryError> {
        use crate::query::lexer::with_lexer;

        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(ParseError::EmptyQuery.into());
        }

        // Quick check: no pipe → no pipeline
        if !trimmed.contains('|') {
            return Ok(None);
        }

        let tokens = with_lexer(input, |batch| Ok(batch.into_vec()))?;

        let mut parser = Parser::new(tokens);
        let pipeline = parser.parse_pipeline()?;
        Ok(pipeline)
    }

    /// Parse the token stream into a Query AST
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] when the token stream is invalid.
    pub fn parse(&mut self) -> Result<Query, ParseError> {
        if self.is_at_end() {
            return Err(ParseError::EmptyQuery);
        }

        let start_span = self.peek().span.clone();
        let root = self.parse_join()?;

        // Ensure we've consumed all tokens except EOF and Pipe
        if !self.is_at_end() && !matches!(self.peek().token_type, TokenType::Pipe) {
            let token = self.peek();
            return Err(ParseError::UnexpectedToken {
                token: token.clone(),
                expected: "end of query".to_string(),
            });
        }

        let end_span = self.tokens[self.position - 1].span.clone();
        let span = start_span.merge(&end_span);

        Ok(Query { root, span })
    }

    /// Parse a pipeline query: `base_query | stage1 | stage2 ...`
    ///
    /// Returns `None` if no pipe operators are found.
    fn parse_pipeline(&mut self) -> Result<Option<PipelineQuery>, ParseError> {
        if self.is_at_end() {
            return Err(ParseError::EmptyQuery);
        }

        let start_span = self.peek().span.clone();
        let query = self.parse()?;

        // Check for pipe
        if !self.match_token(&TokenType::Pipe) {
            return Ok(None);
        }

        let mut stages = Vec::new();
        stages.push(self.parse_pipeline_stage()?);

        while self.match_token(&TokenType::Pipe) {
            stages.push(self.parse_pipeline_stage()?);
        }

        if !self.is_at_end() {
            let token = self.peek();
            return Err(ParseError::UnexpectedToken {
                token: token.clone(),
                expected: "end of query or '|'".to_string(),
            });
        }

        let end_span = self.tokens[self.position - 1].span.clone();
        let span = start_span.merge(&end_span);

        Ok(Some(PipelineQuery {
            query,
            stages,
            span,
        }))
    }

    /// Parse a single pipeline stage: `count`, `group_by <field>`, `top <N> <field>`, `stats`
    fn parse_pipeline_stage(&mut self) -> Result<PipelineStage, ParseError> {
        let token = self.advance().clone();
        match &token.token_type {
            TokenType::Word(w) | TokenType::Identifier(w) => match w.to_lowercase().as_str() {
                "count" => Ok(PipelineStage::Count),
                "stats" => Ok(PipelineStage::Stats),
                "group_by" => {
                    let field_token = self.advance().clone();
                    let field_name = match &field_token.token_type {
                        TokenType::Word(f) | TokenType::Identifier(f) => f.clone(),
                        _ => {
                            return Err(ParseError::InvalidSyntax {
                                message: "Expected field name after 'group_by'".to_string(),
                                span: field_token.span,
                            });
                        }
                    };
                    Ok(PipelineStage::GroupBy {
                        field: Field::new(field_name),
                    })
                }
                "top" => {
                    let n_token = self.advance().clone();
                    let n = match &n_token.token_type {
                        TokenType::NumberLiteral(n) => {
                            usize::try_from(*n).map_err(|_| ParseError::InvalidSyntax {
                                message: format!("Invalid count for 'top': {n}"),
                                span: n_token.span.clone(),
                            })?
                        }
                        _ => {
                            return Err(ParseError::InvalidSyntax {
                                message: "Expected number after 'top'".to_string(),
                                span: n_token.span,
                            });
                        }
                    };
                    let field_token = self.advance().clone();
                    let field_name = match &field_token.token_type {
                        TokenType::Word(f) | TokenType::Identifier(f) => f.clone(),
                        _ => {
                            return Err(ParseError::InvalidSyntax {
                                message: "Expected field name after 'top <N>'".to_string(),
                                span: field_token.span,
                            });
                        }
                    };
                    Ok(PipelineStage::Top {
                        n,
                        field: Field::new(field_name),
                    })
                }
                other => Err(ParseError::InvalidSyntax {
                    message: format!(
                        "Unknown pipeline stage: '{other}'. Expected: count, group_by, top, stats"
                    ),
                    span: token.span,
                }),
            },
            _ => Err(ParseError::InvalidSyntax {
                message: "Expected pipeline stage name after '|'".to_string(),
                span: token.span,
            }),
        }
    }

    /// Parse join expression (lowest precedence, above OR)
    /// `join_expr` ::= `or_expr` (CALLS | IMPORTS | INHERITS | IMPLEMENTS) `or_expr`
    fn parse_join(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_or()?;

        // Check for join keyword (case-insensitive word matching)
        let join_edge = match &self.peek().token_type {
            TokenType::Word(w) => match w.to_uppercase().as_str() {
                "CALLS" => Some(JoinEdgeKind::Calls),
                "IMPORTS" => Some(JoinEdgeKind::Imports),
                "INHERITS" => Some(JoinEdgeKind::Inherits),
                "IMPLEMENTS" => Some(JoinEdgeKind::Implements),
                _ => None,
            },
            _ => None,
        };

        if let Some(edge) = join_edge {
            let join_token = self.advance().clone();
            let right = self.parse_or()?;

            let span = if let Some(left_span) = expr_span(&left) {
                left_span.merge(&join_token.span)
            } else {
                join_token.span
            };

            Ok(Expr::Join(JoinExpr {
                left: Box::new(left),
                edge,
                right: Box::new(right),
                span,
            }))
        } else {
            Ok(left)
        }
    }

    /// Parse OR expression (lowest precedence)
    /// `or_expr` ::= `and_expr` (OR `and_expr`)*
    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut operands = vec![self.parse_and()?];

        while self.match_token(&TokenType::Or) {
            operands.push(self.parse_and()?);
        }

        if operands.len() == 1 {
            operands
                .into_iter()
                .next()
                .ok_or_else(|| ParseError::InvalidSyntax {
                    message: "Internal parser error: OR expression has no operands (this should never happen)".to_string(),
                    // Span is placeholder since this error is impossible and has no meaningful location
                    span: Span::new(0, 0),
                })
        } else {
            Ok(Expr::Or(operands))
        }
    }

    /// Parse AND expression (medium precedence)
    /// `and_expr` ::= `not_expr` (AND `not_expr`)*
    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut operands = vec![self.parse_not()?];

        while self.match_token(&TokenType::And) {
            operands.push(self.parse_not()?);
        }

        if operands.len() == 1 {
            operands
                .into_iter()
                .next()
                .ok_or_else(|| ParseError::InvalidSyntax {
                    message: "Internal parser error: AND expression has no operands (this should never happen)".to_string(),
                    // Span is placeholder since this error is impossible and has no meaningful location
                    span: Span::new(0, 0),
                })
        } else {
            Ok(Expr::And(operands))
        }
    }

    /// Parse NOT expression (highest precedence)
    /// `not_expr` ::= NOT `not_expr` | primary
    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if self.match_token(&TokenType::Not) {
            let operand = self.parse_not()?; // Right-associative
            Ok(Expr::Not(Box::new(operand)))
        } else {
            self.parse_primary()
        }
    }

    /// Parse primary: condition or grouped expression
    /// primary ::= LPAREN `or_expr` RPAREN | condition
    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        // Check for parentheses
        if self.match_token(&TokenType::LParen) {
            let lparen_span = self.tokens[self.position - 1].span.clone();
            let expr = self.parse_or()?;

            if !self.match_token(&TokenType::RParen) {
                return Err(ParseError::UnmatchedParen {
                    open_span: lparen_span,
                    eof: self.is_at_end(),
                });
            }

            return Ok(expr);
        }

        // Shorthand: bare word/string/regex/variable defaults to name predicate
        let token_type = self.peek().token_type.clone();
        match token_type {
            TokenType::Word(word) => {
                let token = self.advance().clone();
                Ok(Expr::Condition(Condition {
                    field: Field::new("name"),
                    operator: Operator::Regex,
                    value: Value::Regex(RegexValue {
                        pattern: word,
                        flags: RegexFlags::default(),
                    }),
                    span: token.span,
                }))
            }
            TokenType::StringLiteral(value) => {
                let token = self.advance().clone();
                Ok(Expr::Condition(Condition {
                    field: Field::new("name"),
                    operator: Operator::Equal,
                    value: Value::String(value),
                    span: token.span,
                }))
            }
            TokenType::RegexLiteral { pattern, flags } => {
                let token = self.advance().clone();
                Ok(Expr::Condition(Condition {
                    field: Field::new("name"),
                    operator: Operator::Regex,
                    value: Value::Regex(RegexValue { pattern, flags }),
                    span: token.span,
                }))
            }
            TokenType::Variable(name) => {
                let token = self.advance().clone();
                Ok(Expr::Condition(Condition {
                    field: Field::new("name"),
                    operator: Operator::Equal,
                    value: Value::Variable(name),
                    span: token.span,
                }))
            }
            _ => self.parse_condition(),
        }
    }

    /// Parse condition: field operator value
    /// condition ::= identifier operator value
    ///
    /// For relation fields (callers, callees, imports, exports, impl, references),
    /// a `(` after `:` introduces a subquery value.
    fn parse_condition(&mut self) -> Result<Expr, ParseError> {
        // Parse field name
        let field_token = self.advance().clone();
        let field = match &field_token.token_type {
            TokenType::Identifier(name) => Field::new(name.clone()),
            _ => {
                return Err(ParseError::ExpectedIdentifier { token: field_token });
            }
        };

        // Parse operator
        let operator_token = self.advance().clone();
        let mut operator = match &operator_token.token_type {
            TokenType::Colon => Operator::Equal,
            TokenType::RegexOp => Operator::Regex,
            TokenType::Greater => Operator::Greater,
            TokenType::Less => Operator::Less,
            TokenType::GreaterEq => Operator::GreaterEq,
            TokenType::LessEq => Operator::LessEq,
            _ => {
                return Err(ParseError::ExpectedOperator {
                    token: operator_token,
                });
            }
        };

        // Check for subquery: relation_field:(inner_expression)
        let is_relation = crate::query::types::is_relation_field(field.as_str());
        if is_relation
            && operator == Operator::Equal
            && matches!(self.peek().token_type, TokenType::LParen)
        {
            let lparen_span = self.peek().span.clone();
            self.advance(); // consume '('
            let inner_expr = self.parse_or()?;
            if !self.match_token(&TokenType::RParen) {
                return Err(ParseError::UnmatchedParen {
                    open_span: lparen_span,
                    eof: self.is_at_end(),
                });
            }

            let end_span = self.tokens[self.position - 1].span.clone();
            let span = field_token.span.merge(&end_span);

            return Ok(Expr::Condition(Condition {
                field,
                operator,
                value: Value::Subquery(Box::new(inner_expr)),
                span,
            }));
        }

        // Parse value
        let value_token = self.advance().clone();
        let mut value = match &value_token.token_type {
            TokenType::StringLiteral(s) | TokenType::Word(s) => Value::String(s.clone()),
            TokenType::RegexLiteral { pattern, flags } => Value::Regex(RegexValue {
                pattern: pattern.clone(),
                flags: flags.clone(),
            }),
            TokenType::NumberLiteral(n) => Value::Number(*n),
            TokenType::BooleanLiteral(b) => Value::Boolean(*b),
            TokenType::Variable(name) => Value::Variable(name.clone()),
            _ => {
                return Err(ParseError::ExpectedValue { token: value_token });
            }
        };

        if operator == Operator::Regex {
            if let Value::String(pattern) = &value {
                value = Value::Regex(RegexValue {
                    pattern: pattern.clone(),
                    flags: RegexFlags::default(),
                });
            }
        } else if operator == Operator::Equal && matches!(value, Value::Regex(_)) {
            operator = Operator::Regex;
        }

        let span = field_token.span.merge(&value_token.span);

        Ok(Expr::Condition(Condition {
            field,
            operator,
            value,
            span,
        }))
    }

    /// Check if current token matches the given type and consume it
    fn match_token(&mut self, token_type: &TokenType) -> bool {
        if self.check(token_type) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// Check if current token matches the given type without consuming
    fn check(&self, token_type: &TokenType) -> bool {
        if self.is_at_end() {
            return false;
        }

        std::mem::discriminant(&self.peek().token_type) == std::mem::discriminant(token_type)
    }

    /// Advance to the next token and return the previous one
    fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.position += 1;
        }
        &self.tokens[self.position - 1]
    }

    /// Check if we're at the end of the token stream
    fn is_at_end(&self) -> bool {
        matches!(self.peek().token_type, TokenType::Eof)
    }

    /// Peek at the current token without consuming
    fn peek(&self) -> &Token {
        &self.tokens[self.position]
    }
}

fn count_conditions(expr: &Expr) -> usize {
    match expr {
        Expr::And(operands) | Expr::Or(operands) => operands.iter().map(count_conditions).sum(),
        Expr::Not(operand) => count_conditions(operand),
        Expr::Condition(cond) => {
            // Count subquery conditions recursively
            if let Value::Subquery(inner) = &cond.value {
                1 + count_conditions(inner)
            } else {
                1
            }
        }
        Expr::Join(join) => count_conditions(&join.left) + count_conditions(&join.right),
    }
}

/// Extract span from an expression (best-effort for error reporting).
fn expr_span(expr: &Expr) -> Option<Span> {
    match expr {
        Expr::Condition(c) => Some(c.span.clone()),
        Expr::Join(j) => Some(j.span.clone()),
        Expr::And(ops) | Expr::Or(ops) => {
            let first = ops.first().and_then(expr_span)?;
            let last = ops.last().and_then(expr_span)?;
            Some(first.merge(&last))
        }
        Expr::Not(inner) => expr_span(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::lexer::with_lexer;
    use crate::query::types::RegexFlags;

    fn parse(input: &str) -> Result<Query, ParseError> {
        let tokens = with_lexer(input, |batch| Ok(batch.into_vec())).unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse()
    }

    #[test]
    fn parse_query_reuses_thread_local_pool() {
        crate::query::lexer::configure_pool_for_tests(
            4,
            crate::query::lexer::ShrinkPolicy::default(),
        );

        let (before_stash, before_in_flight, _) = crate::query::lexer::pool_stats_for_tests();
        assert_eq!(before_stash, 0);
        assert_eq!(before_in_flight, 0);

        parse("kind:function").unwrap();
        parse("name:test").unwrap();

        let (after_stash, after_in_flight, max_size) = crate::query::lexer::pool_stats_for_tests();
        assert!(max_size >= 1);
        assert!(after_stash >= 1);
        assert_eq!(after_in_flight, 0);

        crate::query::lexer::reset_pool_to_default_for_tests();
    }

    #[test]
    fn test_parse_simple_condition() {
        let query = parse("kind:function").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "kind");
                assert_eq!(cond.operator, Operator::Equal);
                assert!(matches!(cond.value, Value::String(ref s) if s == "function"));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_and() {
        let query = parse("kind:function AND async:true").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "kind");
                    }
                    _ => panic!("Expected Condition"),
                }

                match &operands[1] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "async");
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parse_or() {
        let query = parse("kind:function OR kind:class").unwrap();

        match query.root {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 2);
            }
            _ => panic!("Expected Or"),
        }
    }

    #[test]
    fn test_parse_not() {
        let query = parse("NOT kind:function").unwrap();

        match query.root {
            Expr::Not(operand) => match *operand {
                Expr::Condition(cond) => {
                    assert_eq!(cond.field.as_str(), "kind");
                }
                _ => panic!("Expected Condition inside Not"),
            },
            _ => panic!("Expected Not"),
        }
    }

    #[test]
    fn test_precedence_or_and() {
        // A OR B AND C should parse as: Or(A, And(B, C))
        let query = parse("a:1 OR b:2 AND c:3").unwrap();

        match query.root {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 2);

                // First operand should be a condition
                assert!(matches!(operands[0], Expr::Condition(_)));

                // Second operand should be an AND
                match &operands[1] {
                    Expr::And(and_operands) => {
                        assert_eq!(and_operands.len(), 2);
                    }
                    _ => panic!("Expected And as second operand of Or"),
                }
            }
            _ => panic!("Expected Or"),
        }
    }

    #[test]
    fn test_precedence_not_and() {
        // NOT A AND B should parse as: And(Not(A), B)
        let query = parse("NOT a:1 AND b:2").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                // First operand should be NOT
                assert!(matches!(operands[0], Expr::Not(_)));

                // Second operand should be a condition
                assert!(matches!(operands[1], Expr::Condition(_)));
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parentheses_simple() {
        let query = parse("(kind:function)").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "kind");
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parentheses_override_precedence() {
        // (A OR B) AND C should parse as: And(Or(A, B), C)
        let query = parse("(a:1 OR b:2) AND c:3").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                // First operand should be OR
                match &operands[0] {
                    Expr::Or(or_operands) => {
                        assert_eq!(or_operands.len(), 2);
                    }
                    _ => panic!("Expected Or as first operand"),
                }

                // Second operand should be a condition
                assert!(matches!(operands[1], Expr::Condition(_)));
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_nested_parentheses() {
        let query = parse("((a:1))").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "a");
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_complex_query() {
        let query =
            parse("(kind:function OR kind:method) AND async:true AND NOT name~=/test/").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 3);

                // First: (kind:function OR kind:method)
                assert!(matches!(operands[0], Expr::Or(_)));

                // Second: async:true
                assert!(matches!(operands[1], Expr::Condition(_)));

                // Third: NOT name~=/test/
                assert!(matches!(operands[2], Expr::Not(_)));
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_chained_and() {
        let query = parse("a:1 AND b:2 AND c:3 AND d:4").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 4);
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_chained_or() {
        let query = parse("a:1 OR b:2 OR c:3 OR d:4").unwrap();

        match query.root {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 4);
            }
            _ => panic!("Expected Or"),
        }
    }

    #[test]
    fn test_double_not() {
        let query = parse("NOT NOT kind:function").unwrap();

        match query.root {
            Expr::Not(operand) => match *operand {
                Expr::Not(inner) => match *inner {
                    Expr::Condition(_) => {}
                    _ => panic!("Expected Condition"),
                },
                _ => panic!("Expected Not"),
            },
            _ => panic!("Expected Not"),
        }
    }

    #[test]
    fn test_triple_not() {
        let query = parse("NOT NOT NOT kind:function").unwrap();

        match query.root {
            Expr::Not(operand1) => match *operand1 {
                Expr::Not(operand2) => match *operand2 {
                    Expr::Not(operand3) => match *operand3 {
                        Expr::Condition(_) => {}
                        _ => panic!("Expected Condition"),
                    },
                    _ => panic!("Expected Not"),
                },
                _ => panic!("Expected Not"),
            },
            _ => panic!("Expected Not"),
        }
    }

    #[test]
    fn test_operators_all() {
        // Test all operators
        let _ = parse("a:b").unwrap(); // Equal
        let _ = parse("a~=/b/").unwrap(); // Regex
        let _ = parse("a>1").unwrap(); // Greater
        let _ = parse("a<1").unwrap(); // Less
        let _ = parse("a>=1").unwrap(); // GreaterEq
        let _ = parse("a<=1").unwrap(); // LessEq
    }

    #[test]
    fn test_value_types() {
        // String literal
        let query = parse(r#"name:"hello world""#).unwrap();
        match query.root {
            Expr::Condition(cond) => {
                assert!(matches!(cond.value, Value::String(ref s) if s == "hello world"));
            }
            _ => panic!("Expected Condition"),
        }

        // Number
        let query = parse("lines:42").unwrap();
        match query.root {
            Expr::Condition(cond) => {
                assert!(matches!(cond.value, Value::Number(42)));
            }
            _ => panic!("Expected Condition"),
        }

        // Boolean
        let query = parse("async:true").unwrap();
        match query.root {
            Expr::Condition(cond) => {
                assert!(matches!(cond.value, Value::Boolean(true)));
            }
            _ => panic!("Expected Condition"),
        }

        // Regex
        let query = parse("name~=/^test_/i").unwrap();
        match query.root {
            Expr::Condition(cond) => match &cond.value {
                Value::Regex(regex) => {
                    assert_eq!(regex.pattern, "^test_");
                    assert!(regex.flags.case_insensitive);
                }
                _ => panic!("Expected Regex value"),
            },
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_plain_word_defaults_to_name_regex() {
        let query = parse("Error").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "name");
                assert_eq!(cond.operator, Operator::Regex);
                match cond.value {
                    Value::Regex(regex) => {
                        assert_eq!(regex.pattern, "Error");
                        assert_eq!(regex.flags, RegexFlags::default());
                    }
                    _ => panic!("Expected regex value"),
                }
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_plain_string_literal_defaults_to_name_equal() {
        let query = parse(r#""Error""#).unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "name");
                assert_eq!(cond.operator, Operator::Equal);
                match cond.value {
                    Value::String(value) => assert_eq!(value, "Error"),
                    _ => panic!("Expected string value"),
                }
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_error_empty_query() {
        let result = parse("");
        assert!(matches!(result, Err(ParseError::EmptyQuery)));
    }

    #[test]
    fn test_error_unmatched_lparen() {
        let result = parse("(kind:function");
        assert!(matches!(result, Err(ParseError::UnmatchedParen { .. })));
    }

    #[test]
    fn test_error_unmatched_rparen() {
        let result = parse("kind:function)");
        assert!(matches!(result, Err(ParseError::UnexpectedToken { .. })));
    }

    #[test]
    fn test_error_missing_value() {
        let result = parse("kind:");
        assert!(matches!(result, Err(ParseError::ExpectedValue { .. })));
    }

    #[test]
    fn test_error_incomplete_and() {
        let result = parse("kind:function AND");
        assert!(matches!(result, Err(ParseError::ExpectedIdentifier { .. })));
    }

    #[test]
    fn test_error_incomplete_or() {
        let result = parse("kind:function OR");
        assert!(matches!(result, Err(ParseError::ExpectedIdentifier { .. })));
    }

    #[test]
    fn test_error_incomplete_not() {
        let result = parse("NOT");
        assert!(matches!(result, Err(ParseError::ExpectedIdentifier { .. })));
    }

    #[test]
    fn test_error_missing_operator() {
        // Legacy syntax without an explicit operator should still fail,
        // leaving the trailing token unconsumed and triggering an error.
        let result = parse("kind function");
        assert!(matches!(result, Err(ParseError::UnexpectedToken { .. })));
    }

    #[test]
    fn test_precedence_complex_1() {
        // A OR B AND C OR D should parse as: Or(A, And(B, C), D)
        let query = parse("a:1 OR b:2 AND c:3 OR d:4").unwrap();

        match query.root {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 3);

                // First operand: condition
                assert!(matches!(operands[0], Expr::Condition(_)));

                // Second operand: AND
                assert!(matches!(operands[1], Expr::And(_)));

                // Third operand: condition
                assert!(matches!(operands[2], Expr::Condition(_)));
            }
            _ => panic!("Expected Or"),
        }
    }

    #[test]
    fn test_precedence_complex_2() {
        // NOT A OR B AND C should parse as: Or(Not(A), And(B, C))
        let query = parse("NOT a:1 OR b:2 AND c:3").unwrap();

        match query.root {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 2);

                // First operand: NOT
                assert!(matches!(operands[0], Expr::Not(_)));

                // Second operand: AND
                assert!(matches!(operands[1], Expr::And(_)));
            }
            _ => panic!("Expected Or"),
        }
    }

    #[test]
    fn test_multiple_groups() {
        // (A OR B) AND (C OR D)
        let query = parse("(a:1 OR b:2) AND (c:3 OR d:4)").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                // Both operands should be OR
                assert!(matches!(operands[0], Expr::Or(_)));
                assert!(matches!(operands[1], Expr::Or(_)));
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_not_with_parentheses() {
        // NOT (A OR B)
        let query = parse("NOT (a:1 OR b:2)").unwrap();

        match query.root {
            Expr::Not(operand) => match *operand {
                Expr::Or(or_operands) => {
                    assert_eq!(or_operands.len(), 2);
                }
                _ => panic!("Expected Or inside Not"),
            },
            _ => panic!("Expected Not"),
        }
    }

    #[test]
    fn test_deeply_nested() {
        // ((A AND B) OR (C AND D)) AND E
        let query = parse("((a:1 AND b:2) OR (c:3 AND d:4)) AND e:5").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                // First operand should be OR with AND children
                match &operands[0] {
                    Expr::Or(or_operands) => {
                        assert_eq!(or_operands.len(), 2);
                        assert!(matches!(or_operands[0], Expr::And(_)));
                        assert!(matches!(or_operands[1], Expr::And(_)));
                    }
                    _ => panic!("Expected Or"),
                }

                // Second operand should be condition
                assert!(matches!(operands[1], Expr::Condition(_)));
            }
            _ => panic!("Expected And"),
        }
    }

    // P2-34 Phase 2: Tests for scope.* fields

    #[test]
    fn test_parse_scope_type() {
        let query = parse("scope.type:class").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "scope.type");
                assert_eq!(cond.operator, Operator::Equal);
                assert!(matches!(cond.value, Value::String(ref s) if s == "class"));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_scope_name() {
        let query = parse("scope.name:UserService").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "scope.name");
                assert_eq!(cond.operator, Operator::Equal);
                assert!(matches!(cond.value, Value::String(ref s) if s == "UserService"));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_scope_parent() {
        let query = parse("scope.parent:Database").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "scope.parent");
                assert_eq!(cond.operator, Operator::Equal);
                assert!(matches!(cond.value, Value::String(ref s) if s == "Database"));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_scope_ancestor() {
        let query = parse("scope.ancestor:UserModule").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "scope.ancestor");
                assert_eq!(cond.operator, Operator::Equal);
                assert!(matches!(cond.value, Value::String(ref s) if s == "UserModule"));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_scope_type_with_regex() {
        let query = parse("scope.type~=/class|function/").unwrap();

        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "scope.type");
                assert_eq!(cond.operator, Operator::Regex);
                assert!(matches!(cond.value, Value::Regex(_)));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_scope_and_kind() {
        let query = parse("scope.type:class AND name:connect").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "scope.type");
                    }
                    _ => panic!("Expected Condition"),
                }

                match &operands[1] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "name");
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parse_scope_with_and_composition() {
        // name:connect AND scope.ancestor:UserModule
        let query = parse("name:connect AND scope.ancestor:UserModule").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "name");
                        assert!(matches!(cond.value, Value::String(ref s) if s == "connect"));
                    }
                    _ => panic!("Expected Condition"),
                }

                match &operands[1] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "scope.ancestor");
                        assert!(matches!(cond.value, Value::String(ref s) if s == "UserModule"));
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parse_not_scope_type() {
        // NOT scope.type:test
        let query = parse("NOT scope.type:test").unwrap();

        match query.root {
            Expr::Not(operand) => match *operand {
                Expr::Condition(cond) => {
                    assert_eq!(cond.field.as_str(), "scope.type");
                    assert!(matches!(cond.value, Value::String(ref s) if s == "test"));
                }
                _ => panic!("Expected Condition inside Not"),
            },
            _ => panic!("Expected Not"),
        }
    }

    #[test]
    fn test_parse_complex_scope_composition() {
        // scope.type:class AND (name:connect OR name:disconnect)
        let query = parse("scope.type:class AND (name:connect OR name:disconnect)").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                // First operand should be scope.type condition
                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "scope.type");
                    }
                    _ => panic!("Expected Condition for scope.type"),
                }

                // Second operand should be OR with name conditions
                match &operands[1] {
                    Expr::Or(or_operands) => {
                        assert_eq!(or_operands.len(), 2);
                        match &or_operands[0] {
                            Expr::Condition(cond) => {
                                assert_eq!(cond.field.as_str(), "name");
                                assert!(
                                    matches!(cond.value, Value::String(ref s) if s == "connect")
                                );
                            }
                            _ => panic!("Expected name:connect"),
                        }
                        match &or_operands[1] {
                            Expr::Condition(cond) => {
                                assert_eq!(cond.field.as_str(), "name");
                                assert!(
                                    matches!(cond.value, Value::String(ref s) if s == "disconnect")
                                );
                            }
                            _ => panic!("Expected name:disconnect"),
                        }
                    }
                    _ => panic!("Expected Or for name conditions"),
                }
            }
            _ => panic!("Expected And"),
        }
    }

    #[test]
    fn test_parse_multiple_scope_filters() {
        // scope.type:function AND scope.parent:Database AND scope.ancestor:UserModule
        let query =
            parse("scope.type:function AND scope.parent:Database AND scope.ancestor:UserModule")
                .unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 3);

                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "scope.type");
                    }
                    _ => panic!("Expected scope.type condition"),
                }

                match &operands[1] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "scope.parent");
                    }
                    _ => panic!("Expected scope.parent condition"),
                }

                match &operands[2] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "scope.ancestor");
                    }
                    _ => panic!("Expected scope.ancestor condition"),
                }
            }
            _ => panic!("Expected And"),
        }
    }

    // Tests for invariant validation (Phase 0 Fix #3)
    // Verify that single-operand expressions are NOT wrapped in Or/And

    #[test]
    fn test_single_operand_or_not_wrapped() {
        // Single condition with no OR operators should NOT be wrapped in Or
        let query = parse("kind:function").unwrap();

        match query.root {
            Expr::Condition(_) => {
                // Expected: bare Condition, not Or([Condition])
            }
            Expr::Or(_) => panic!("Single operand should NOT be wrapped in Or expression"),
            _ => panic!("Expected Condition expression"),
        }
    }

    #[test]
    fn test_multiple_operands_or_wrapped() {
        // Multiple conditions with OR should create Or expression
        let query = parse("kind:function OR kind:class").unwrap();

        match query.root {
            Expr::Or(operands) => {
                assert_eq!(
                    operands.len(),
                    2,
                    "OR expression should have exactly 2 operands"
                );
            }
            _ => panic!("Multiple operands with OR should create Or expression"),
        }
    }

    #[test]
    fn test_single_operand_and_not_wrapped() {
        // Single condition with no AND operators should NOT be wrapped in And
        let query = parse("name:test").unwrap();

        match query.root {
            Expr::Condition(_) => {
                // Expected: bare Condition, not And([Condition])
            }
            Expr::And(_) => panic!("Single operand should NOT be wrapped in And expression"),
            _ => panic!("Expected Condition expression"),
        }
    }

    #[test]
    fn test_multiple_operands_and_wrapped() {
        // Multiple conditions with AND should create And expression
        let query = parse("kind:function AND async:true").unwrap();

        match query.root {
            Expr::And(operands) => {
                assert_eq!(
                    operands.len(),
                    2,
                    "AND expression should have exactly 2 operands"
                );
            }
            _ => panic!("Multiple operands with AND should create And expression"),
        }
    }

    // D3 Advanced Query Feature tests

    #[test]
    fn test_parse_variable_value() {
        let query = parse("kind:$type").unwrap();
        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "kind");
                assert_eq!(cond.operator, Operator::Equal);
                assert!(matches!(cond.value, Value::Variable(ref n) if n == "type"));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_subquery() {
        let query = parse("callers:(kind:function)").unwrap();
        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "callers");
                assert_eq!(cond.operator, Operator::Equal);
                match cond.value {
                    Value::Subquery(inner) => match *inner {
                        Expr::Condition(inner_cond) => {
                            assert_eq!(inner_cond.field.as_str(), "kind");
                            assert!(
                                matches!(inner_cond.value, Value::String(ref s) if s == "function")
                            );
                        }
                        _ => panic!("Expected inner Condition"),
                    },
                    _ => panic!("Expected Subquery value"),
                }
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_parse_non_relation_field_not_subquery() {
        // `(kind:function)` is grouping, not a subquery, because there is no relation field
        let query = parse("(kind:function)").unwrap();
        match query.root {
            Expr::Condition(cond) => {
                assert_eq!(cond.field.as_str(), "kind");
                // Value should be a String, not a Subquery
                assert!(matches!(cond.value, Value::String(_)));
            }
            _ => panic!("Expected Condition (grouping), not a subquery"),
        }
    }

    #[test]
    fn test_parse_join_calls() {
        let query = parse("(kind:function) CALLS (kind:function)").unwrap();
        match query.root {
            Expr::Join(join) => {
                assert_eq!(join.edge, JoinEdgeKind::Calls);
                match *join.left {
                    Expr::Condition(ref cond) => assert_eq!(cond.field.as_str(), "kind"),
                    _ => panic!("Expected left Condition"),
                }
                match *join.right {
                    Expr::Condition(ref cond) => assert_eq!(cond.field.as_str(), "kind"),
                    _ => panic!("Expected right Condition"),
                }
            }
            _ => panic!("Expected Join expression"),
        }
    }

    #[test]
    fn test_parse_join_imports() {
        let query = parse("(kind:function) IMPORTS (kind:module)").unwrap();
        match query.root {
            Expr::Join(join) => {
                assert_eq!(join.edge, JoinEdgeKind::Imports);
                match *join.right {
                    Expr::Condition(ref cond) => {
                        assert!(matches!(cond.value, Value::String(ref s) if s == "module"));
                    }
                    _ => panic!("Expected right Condition"),
                }
            }
            _ => panic!("Expected Join expression"),
        }
    }

    #[test]
    fn test_parse_pipeline_count() {
        let pipeline = Parser::parse_pipeline_query("kind:function | count")
            .unwrap()
            .expect("Expected Some(PipelineQuery)");
        assert_eq!(pipeline.stages.len(), 1);
        assert!(matches!(pipeline.stages[0], PipelineStage::Count));
    }

    #[test]
    fn test_parse_pipeline_group_by() {
        let pipeline = Parser::parse_pipeline_query("kind:function | group_by lang")
            .unwrap()
            .expect("Expected Some(PipelineQuery)");
        assert_eq!(pipeline.stages.len(), 1);
        match &pipeline.stages[0] {
            PipelineStage::GroupBy { field } => assert_eq!(field.as_str(), "lang"),
            other => panic!("Expected GroupBy, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_pipeline_top() {
        let pipeline = Parser::parse_pipeline_query("kind:function | top 10 lang")
            .unwrap()
            .expect("Expected Some(PipelineQuery)");
        assert_eq!(pipeline.stages.len(), 1);
        match &pipeline.stages[0] {
            PipelineStage::Top { n, field } => {
                assert_eq!(*n, 10);
                assert_eq!(field.as_str(), "lang");
            }
            other => panic!("Expected Top, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_pipeline_stats() {
        let pipeline = Parser::parse_pipeline_query("kind:function | stats")
            .unwrap()
            .expect("Expected Some(PipelineQuery)");
        assert_eq!(pipeline.stages.len(), 1);
        assert!(matches!(pipeline.stages[0], PipelineStage::Stats));
    }
}
