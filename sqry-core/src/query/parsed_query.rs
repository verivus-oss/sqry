//! `ParsedQuery` - canonical AST representation for indexed and non-indexed flows
//!
//! This module provides the `ParsedQuery` struct which encapsulates:
//! - The parsed `QueryAST` (Arc-wrapped for zero-copy sharing)
//! - Extracted `RepoFilter` for workspace/multi-repo queries
//! - Normalized query string (repo predicates stripped for cache key normalization)
//!
//! # Purpose
//!
//! `ParsedQuery` serves as the unified representation used by:
//! - `QueryExecutor::execute_with_index_ast` (indexed queries)
//! - `QueryExecutor::execute_query_with_stats` (non-indexed filesystem queries)
//! - `SessionManager::query` (session-based cached queries)
//! - `WorkspaceIndex::query` (multi-repo workspace queries)
//!
//! By extracting repo filters and normalizing query strings, we enable:
//! - Better cache hit rates (repo filters don't pollute cache keys)
//! - Consistent repo filtering across all execution paths
//! - Zero-copy AST sharing via Arc

use crate::query::RepoFilter;
use crate::query::types::{Expr, JoinExpr, Operator, Query as QueryAST, Value};
use anyhow::Result;
use std::sync::Arc;

/// Canonical parsed query representation
///
/// Contains the boolean query AST, extracted repo filter, and normalized
/// query string for cache key generation.
///
/// # Example
///
/// ```ignore
/// use sqry_core::query::QueryExecutor;
///
/// let executor = QueryExecutor::new();
/// let parsed = executor.parse_query_ast("repo:backend-* AND kind:function")?;
///
/// // AST contains the boolean expression (without repo predicates)
/// let ast = &parsed.ast;
///
/// // Repo filter extracted for workspace/multi-repo filtering
/// let repo_filter = &parsed.repo_filter;
///
/// // Normalized string used for cache keys (repo predicates stripped)
/// let cache_key_string = &parsed.normalized;
/// ```
#[derive(Debug, Clone)]
pub struct ParsedQuery {
    /// Parsed boolean query AST (Arc-wrapped for zero-copy sharing)
    ///
    /// This AST represents the query after:
    /// - Lexing and parsing
    /// - Validation against field registry
    /// - Optimization (if enabled)
    /// - Repo predicate extraction (repo conditions removed from AST)
    pub ast: Arc<QueryAST>,

    /// Extracted repository filter
    ///
    /// Compiled from `repo:` predicates in the original query.
    /// Used by workspace and session managers to filter repositories
    /// before executing the query.
    ///
    /// Empty filter matches all repositories.
    pub repo_filter: RepoFilter,

    /// Normalized query string (repo predicates stripped)
    ///
    /// Used for cache key generation. By stripping repo predicates,
    /// we improve cache hit rates across different repo filters.
    ///
    /// Example:
    /// - Input: "repo:backend-* AND kind:function AND name:test"
    /// - Normalized: "kind:function AND name:test"
    ///
    /// `Arc<str>` provides zero-copy sharing in cache keys.
    pub normalized: Arc<str>,
}

impl ParsedQuery {
    /// Create a new `ParsedQuery`
    ///
    /// # Arguments
    ///
    /// * `ast` - Parsed boolean query AST
    /// * `repo_filter` - Extracted repository filter
    /// * `normalized` - Normalized query string (repo predicates stripped)
    #[must_use]
    pub fn new(ast: Arc<QueryAST>, repo_filter: RepoFilter, normalized: String) -> Self {
        Self {
            ast,
            repo_filter,
            normalized: Arc::from(normalized),
        }
    }

    /// Create `ParsedQuery` from an AST
    ///
    /// This is a convenience method that:
    /// 1. Extracts repo filter from AST
    /// 2. Strips repo predicates to create normalized AST
    /// 3. Serializes normalized AST to string for cache key
    ///
    /// # Arguments
    ///
    /// * `ast` - Parsed boolean query AST (Arc-wrapped)
    ///
    /// # Returns
    ///
    /// * `Ok(ParsedQuery)` - Successfully created `ParsedQuery`
    /// * `Err(...)` - Failed to extract repo filter or normalize AST
    ///
    /// # Example
    ///
    /// ```ignore
    /// use sqry_core::query::{QueryParser, ParsedQuery};
    /// use std::sync::Arc;
    ///
    /// let ast = QueryParser::parse_query("kind:function AND name:test")?;
    /// let parsed = ParsedQuery::from_ast(Arc::new(ast))?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`anyhow::Error`] when repo filter extraction or normalization fails.
    pub fn from_ast(ast: Arc<QueryAST>) -> Result<Self> {
        // Extract repo filter
        let repo_filter = extract_repo_filter(&ast)?;

        // Strip repo predicates for normalization
        let normalized_ast = strip_repo_predicates(&ast);

        // Serialize normalized AST to string for cache key
        let normalized = if let Some(norm_ast) = normalized_ast {
            serialize_query(&norm_ast)
        } else {
            // Query is only repo predicates, normalize to empty string
            String::new()
        };

        Ok(Self {
            ast,
            repo_filter,
            normalized: Arc::from(normalized),
        })
    }

    /// Get a reference to the AST
    #[inline]
    #[must_use]
    pub fn ast(&self) -> &Arc<QueryAST> {
        &self.ast
    }

    /// Get a reference to the repo filter
    #[inline]
    #[must_use]
    pub fn repo_filter(&self) -> &RepoFilter {
        &self.repo_filter
    }

    /// Get the normalized query string
    #[inline]
    #[must_use]
    pub fn normalized(&self) -> &str {
        &self.normalized
    }
}

/// Extract repo filter patterns from a `QueryAST`
///
/// Traverses the AST and collects all `repo:` predicates into a `RepoFilter`.
/// Returns an empty filter if no repo predicates are found OR if any repo
/// predicate appears in a negated context (NOT).
///
/// # Negation Handling
///
/// If the query contains `NOT repo:foo`, we CANNOT use repo-based pre-filtering
/// because `RepoFilter` only supports positive patterns, not exclusions. In such
/// cases, we return an empty filter (matches all repos) and let the executor
/// handle repo filtering during post-filtering.
///
/// # Examples
///
/// ```ignore
/// // Positive repo filter (works)
/// let ast = QueryParser::parse_query("repo:backend-* AND kind:function")?;
/// let filter = extract_repo_filter(&ast)?;
/// assert_eq!(filter.patterns().len(), 1);
///
/// // Negated repo filter (returns empty filter)
/// let ast = QueryParser::parse_query("NOT repo:test-* AND kind:function")?;
/// let filter = extract_repo_filter(&ast)?;
/// assert_eq!(filter.patterns().len(), 0); // Cannot pre-filter
/// ```
///
/// # Errors
///
/// Returns [`anyhow::Error`] when repo filter construction fails.
pub fn extract_repo_filter(ast: &QueryAST) -> Result<RepoFilter> {
    let mut patterns = Vec::new();
    let has_negated_repo = collect_repo_patterns(&ast.root, &mut patterns, false);

    if has_negated_repo {
        // Found repo predicate in negated context - cannot use pre-filtering
        // Return empty filter (matches all repos)
        RepoFilter::new(vec![])
    } else {
        RepoFilter::new(patterns)
    }
}

/// Recursively collect repo: patterns from an expression
///
/// Returns true if any repo predicate is found in a negated context.
///
/// # Arguments
///
/// * `expr` - Expression to traverse
/// * `patterns` - Vector to collect positive repo patterns
/// * `is_negated` - Whether we're currently in a NOT context
fn collect_repo_patterns(expr: &Expr, patterns: &mut Vec<String>, is_negated: bool) -> bool {
    match expr {
        Expr::And(operands) | Expr::Or(operands) => {
            let mut found_negated = false;
            for operand in operands {
                if collect_repo_patterns(operand, patterns, is_negated) {
                    found_negated = true;
                }
            }
            found_negated
        }
        Expr::Not(operand) => {
            // Flip negation flag when entering NOT
            collect_repo_patterns(operand, patterns, !is_negated)
        }
        Expr::Condition(condition) => {
            if condition.field.as_str() == "repo" {
                if is_negated {
                    // Found repo predicate in negated context - abort pre-filtering
                    return true;
                }

                // Only extract string values (Equal operator with string value)
                if let (Operator::Equal, Value::String(pattern)) =
                    (&condition.operator, &condition.value)
                {
                    patterns.push(pattern.clone());
                }
            }
            false
        }
        Expr::Join(join) => {
            let left = collect_repo_patterns(&join.left, patterns, is_negated);
            let right = collect_repo_patterns(&join.right, patterns, is_negated);
            left || right
        }
    }
}

/// Strip repo predicates from a `QueryAST`, returning a normalized AST
///
/// Creates a new AST with all `repo:` conditions removed. This normalized
/// AST is used for cache keys to improve hit rates across different repo filters.
///
/// # Examples
///
/// ```ignore
/// // Input: repo:backend-* AND kind:function
/// // Output: kind:function
/// let ast = QueryParser::parse_query("repo:backend-* AND kind:function")?;
/// let normalized_ast = strip_repo_predicates(&ast);
/// ```
///
/// # Behavior
///
/// - Removes repo conditions from AND/OR operand lists
/// - Simplifies single-operand AND/OR to the operand itself
/// - Returns None if the entire query consists only of repo predicates
/// - NOT(repo:...) is removed entirely (repo filters don't support NOT)
#[must_use]
pub fn strip_repo_predicates(ast: &QueryAST) -> Option<QueryAST> {
    strip_repo_from_expr(&ast.root).map(|root| QueryAST {
        root,
        span: ast.span.clone(),
    })
}

/// Recursively strip repo predicates from an expression
fn strip_repo_from_expr(expr: &Expr) -> Option<Expr> {
    match expr {
        Expr::And(operands) => {
            let filtered: Vec<Expr> = operands.iter().filter_map(strip_repo_from_expr).collect();

            match filtered.len() {
                0 => None,                                       // All operands were repo predicates
                1 => Some(filtered.into_iter().next().unwrap()), // Simplify AND with single operand
                _ => Some(Expr::And(filtered)),
            }
        }
        Expr::Or(operands) => {
            let filtered: Vec<Expr> = operands.iter().filter_map(strip_repo_from_expr).collect();

            match filtered.len() {
                0 => None,                                       // All operands were repo predicates
                1 => Some(filtered.into_iter().next().unwrap()), // Simplify OR with single operand
                _ => Some(Expr::Or(filtered)),
            }
        }
        Expr::Not(operand) => {
            // Strip repo from the NOT operand
            strip_repo_from_expr(operand).map(|expr| Expr::Not(Box::new(expr)))
        }
        Expr::Condition(condition) => {
            // Filter out repo conditions
            if condition.field.as_str() == "repo" {
                None
            } else {
                Some(Expr::Condition(condition.clone()))
            }
        }
        Expr::Join(join) => {
            let left = strip_repo_from_expr(&join.left)?;
            let right = strip_repo_from_expr(&join.right)?;
            Some(Expr::Join(JoinExpr {
                left: Box::new(left),
                edge: join.edge.clone(),
                right: Box::new(right),
                span: join.span.clone(),
            }))
        }
    }
}

/// Serialize a `QueryAST` back to a query string (for normalized cache keys)
///
/// This is a simplified serializer that produces canonical query strings.
/// NOT guaranteed to preserve exact formatting or parentheses from original input.
///
/// # Example
///
/// ```ignore
/// let ast = QueryParser::parse_query("kind:function AND name:test")?;
/// let query_str = serialize_query(&ast);
/// assert_eq!(query_str, "kind:function AND name:test");
/// ```
#[must_use]
pub fn serialize_query(ast: &QueryAST) -> String {
    serialize_expr(&ast.root)
}

/// Recursively serialize an expression to a query string
fn serialize_expr(expr: &Expr) -> String {
    match expr {
        Expr::And(operands) => {
            let parts: Vec<String> = operands.iter().map(serialize_expr).collect();
            parts.join(" AND ")
        }
        Expr::Or(operands) => {
            let parts: Vec<String> = operands.iter().map(serialize_expr).collect();
            format!("({})", parts.join(" OR "))
        }
        Expr::Not(operand) => {
            format!("NOT {}", serialize_expr(operand))
        }
        Expr::Condition(condition) => {
            let op_str = match condition.operator {
                Operator::Equal => ":",
                Operator::Regex => "~=",
                Operator::Greater => ">",
                Operator::GreaterEq => ">=",
                Operator::Less => "<",
                Operator::LessEq => "<=",
            };

            let value_str = match &condition.value {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Boolean(b) => b.to_string(),
                Value::Regex(r) => {
                    // Include flags to prevent cache key collisions
                    // e.g., /foo/i, /foo/ms, /foo/ (no flags)
                    let mut flags = String::new();
                    if r.flags.case_insensitive {
                        flags.push('i');
                    }
                    if r.flags.multiline {
                        flags.push('m');
                    }
                    if r.flags.dot_all {
                        flags.push('s');
                    }
                    format!("/{}/{}", r.pattern, flags)
                }
                Value::Variable(name) => format!("${name}"),
                Value::Subquery(expr) => format!("({})", serialize_expr(expr)),
            };

            format!("{}{}{}", condition.field.as_str(), op_str, value_str)
        }
        Expr::Join(join) => {
            format!(
                "({}) {} ({})",
                serialize_expr(&join.left),
                join.edge,
                serialize_expr(&join.right)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::types::{
        Condition, Expr, Field, Operator, RegexFlags, RegexValue, Span, Value,
    };

    fn build_regex_ast(flags: RegexFlags) -> QueryAST {
        QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("name"),
                operator: Operator::Regex,
                value: Value::Regex(RegexValue {
                    pattern: "foo".to_string(),
                    flags,
                }),
                span: Span::new(0, 10),
            }),
            span: Span::new(0, 10),
        }
    }

    fn assert_regex_serialization(flags: RegexFlags, expected: &str) {
        let ast = build_regex_ast(flags);
        let serialized = serialize_query(&ast);
        assert_eq!(serialized, expected);
    }

    #[test]
    fn test_parsed_query_creation() {
        // Create a simple AST: kind:function
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::new(0, 13),
            }),
            span: Span::new(0, 13),
        };

        let repo_filter = RepoFilter::new(vec![]).unwrap();
        let normalized = "kind:function".to_string();

        let parsed = ParsedQuery::new(Arc::new(ast), repo_filter, normalized);

        assert_eq!(parsed.normalized(), "kind:function");
        assert!(parsed.repo_filter().patterns().is_empty());
    }

    #[test]
    fn test_parsed_query_with_repo_filter() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::new(0, 13),
            }),
            span: Span::new(0, 13),
        };

        let repo_filter = RepoFilter::new(vec!["backend-*".to_string()]).unwrap();
        let normalized = "kind:function".to_string();

        let parsed = ParsedQuery::new(Arc::new(ast), repo_filter, normalized.clone());

        assert_eq!(parsed.normalized(), "kind:function");
        assert_eq!(parsed.repo_filter().patterns().len(), 1);
        assert_eq!(parsed.repo_filter().patterns()[0], "backend-*");
    }

    #[test]
    fn test_parsed_query_arc_sharing() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("name"),
                operator: Operator::Equal,
                value: Value::String("test".to_string()),
                span: Span::new(0, 10),
            }),
            span: Span::new(0, 10),
        };

        let repo_filter = RepoFilter::new(vec![]).unwrap();
        let parsed = ParsedQuery::new(Arc::new(ast), repo_filter, "name:test".to_string());

        // Clone should share the same Arc
        let parsed_clone = parsed.clone();
        assert!(Arc::ptr_eq(&parsed.ast, &parsed_clone.ast));
        assert!(Arc::ptr_eq(&parsed.normalized, &parsed_clone.normalized));
    }

    #[test]
    fn test_extract_repo_filter_empty() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::new(0, 13),
            }),
            span: Span::new(0, 13),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        assert_eq!(repo_filter.patterns().len(), 0);
    }

    #[test]
    fn test_extract_repo_filter_single() {
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("backend-*".to_string()),
                    span: Span::new(0, 16),
                }),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(21, 34),
                }),
            ]),
            span: Span::new(0, 34),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        assert_eq!(repo_filter.patterns().len(), 1);
        assert_eq!(repo_filter.patterns()[0], "backend-*");
    }

    #[test]
    fn test_extract_repo_filter_multiple() {
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("backend-*".to_string()),
                    span: Span::new(0, 16),
                }),
                Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("frontend-*".to_string()),
                    span: Span::new(21, 38),
                }),
            ]),
            span: Span::new(0, 38),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        assert_eq!(repo_filter.patterns().len(), 2);
        assert!(repo_filter.patterns().contains(&"backend-*".to_string()));
        assert!(repo_filter.patterns().contains(&"frontend-*".to_string()));
    }

    #[test]
    fn test_extract_repo_filter_negated_aborts() {
        // NOT repo:test-* AND kind:function
        // Should return empty filter (cannot pre-filter with negation)
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Not(Box::new(Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("test-*".to_string()),
                    span: Span::new(4, 16),
                }))),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(21, 34),
                }),
            ]),
            span: Span::new(0, 34),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        // Should return empty filter (abort pre-filtering due to negation)
        assert_eq!(
            repo_filter.patterns().len(),
            0,
            "Negated repo predicates should abort pre-filtering"
        );
    }

    #[test]
    fn test_extract_repo_filter_mixed_positive_and_negated() {
        // repo:backend-* AND NOT repo:test-* AND kind:function
        // Has both positive and negated repo predicates
        // Should return empty filter (cannot pre-filter when negation present)
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("backend-*".to_string()),
                    span: Span::new(0, 16),
                }),
                Expr::Not(Box::new(Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("test-*".to_string()),
                    span: Span::new(25, 37),
                }))),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(42, 55),
                }),
            ]),
            span: Span::new(0, 55),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        // Should return empty filter even though there's a positive repo predicate
        assert_eq!(
            repo_filter.patterns().len(),
            0,
            "Mixed positive and negated repo predicates should abort pre-filtering"
        );
    }

    #[test]
    fn test_extract_repo_filter_double_negation() {
        // NOT (NOT repo:backend-*) AND kind:function
        // Double negation = positive context
        // Should extract backend-* pattern
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Not(Box::new(Expr::Not(Box::new(Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("backend-*".to_string()),
                    span: Span::new(9, 25),
                }))))),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(30, 43),
                }),
            ]),
            span: Span::new(0, 43),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        // Double negation = positive, should extract pattern
        assert_eq!(
            repo_filter.patterns().len(),
            1,
            "Double negation should be treated as positive context"
        );
        assert_eq!(repo_filter.patterns()[0], "backend-*");
    }

    #[test]
    fn test_extract_repo_filter_or_with_negation() {
        // (repo:A OR NOT repo:B) AND kind:function
        // Has negated repo in OR - should abort pre-filtering
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Or(vec![
                    Expr::Condition(Condition {
                        field: Field::new("repo"),
                        operator: Operator::Equal,
                        value: Value::String("A".to_string()),
                        span: Span::new(1, 7),
                    }),
                    Expr::Not(Box::new(Expr::Condition(Condition {
                        field: Field::new("repo"),
                        operator: Operator::Equal,
                        value: Value::String("B".to_string()),
                        span: Span::new(16, 22),
                    }))),
                ]),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(28, 41),
                }),
            ]),
            span: Span::new(0, 41),
        };

        let repo_filter = extract_repo_filter(&ast).unwrap();
        assert_eq!(
            repo_filter.patterns().len(),
            0,
            "OR with negated repo should abort pre-filtering"
        );
    }

    #[test]
    fn test_strip_repo_predicates_none() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::new(0, 13),
            }),
            span: Span::new(0, 13),
        };

        let stripped = strip_repo_predicates(&ast).unwrap();
        // Should be unchanged
        if let Expr::Condition(cond) = &stripped.root {
            assert_eq!(cond.field.as_str(), "kind");
        } else {
            panic!("Expected Condition, got {:?}", stripped.root);
        }
    }

    #[test]
    fn test_strip_repo_predicates_and() {
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Condition(Condition {
                    field: Field::new("repo"),
                    operator: Operator::Equal,
                    value: Value::String("backend-*".to_string()),
                    span: Span::new(0, 16),
                }),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(21, 34),
                }),
            ]),
            span: Span::new(0, 34),
        };

        let stripped = strip_repo_predicates(&ast).unwrap();
        // Should simplify to just kind:function
        if let Expr::Condition(cond) = &stripped.root {
            assert_eq!(cond.field.as_str(), "kind");
        } else {
            panic!("Expected simplified Condition, got {:?}", stripped.root);
        }
    }

    #[test]
    fn test_strip_repo_predicates_all_repo() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("repo"),
                operator: Operator::Equal,
                value: Value::String("backend-*".to_string()),
                span: Span::new(0, 16),
            }),
            span: Span::new(0, 16),
        };

        let stripped = strip_repo_predicates(&ast);
        // Should return None (query is only repo predicates)
        assert!(stripped.is_none());
    }

    #[test]
    fn test_serialize_simple_condition() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::new(0, 13),
            }),
            span: Span::new(0, 13),
        };

        let serialized = serialize_query(&ast);
        assert_eq!(serialized, "kind:function");
    }

    #[test]
    fn test_serialize_and_expression() {
        let ast = QueryAST {
            root: Expr::And(vec![
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(0, 13),
                }),
                Expr::Condition(Condition {
                    field: Field::new("name"),
                    operator: Operator::Equal,
                    value: Value::String("test".to_string()),
                    span: Span::new(18, 28),
                }),
            ]),
            span: Span::new(0, 28),
        };

        let serialized = serialize_query(&ast);
        assert_eq!(serialized, "kind:function AND name:test");
    }

    #[test]
    fn test_serialize_or_expression() {
        let ast = QueryAST {
            root: Expr::Or(vec![
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(0, 13),
                }),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("method".to_string()),
                    span: Span::new(17, 28),
                }),
            ]),
            span: Span::new(0, 28),
        };

        let serialized = serialize_query(&ast);
        assert_eq!(serialized, "(kind:function OR kind:method)");
    }

    #[test]
    fn test_serialize_regex_no_flags() {
        assert_regex_serialization(
            RegexFlags {
                case_insensitive: false,
                multiline: false,
                dot_all: false,
            },
            "name~=/foo/",
        );
    }

    #[test]
    fn test_serialize_regex_case_insensitive() {
        assert_regex_serialization(
            RegexFlags {
                case_insensitive: true,
                multiline: false,
                dot_all: false,
            },
            "name~=/foo/i",
        );
    }

    #[test]
    fn test_serialize_regex_multiline() {
        assert_regex_serialization(
            RegexFlags {
                case_insensitive: false,
                multiline: true,
                dot_all: false,
            },
            "name~=/foo/m",
        );
    }

    #[test]
    fn test_serialize_regex_dot_all() {
        assert_regex_serialization(
            RegexFlags {
                case_insensitive: false,
                multiline: false,
                dot_all: true,
            },
            "name~=/foo/s",
        );
    }

    #[test]
    fn test_serialize_regex_all_flags() {
        assert_regex_serialization(
            RegexFlags {
                case_insensitive: true,
                multiline: true,
                dot_all: true,
            },
            "name~=/foo/ims",
        );
    }

    #[test]
    fn test_serialize_regex_flag_combinations_distinct() {
        let no_flags = serialize_query(&build_regex_ast(RegexFlags {
            case_insensitive: false,
            multiline: false,
            dot_all: false,
        }));
        let with_i = serialize_query(&build_regex_ast(RegexFlags {
            case_insensitive: true,
            multiline: false,
            dot_all: false,
        }));
        let with_m = serialize_query(&build_regex_ast(RegexFlags {
            case_insensitive: false,
            multiline: true,
            dot_all: false,
        }));
        let with_s = serialize_query(&build_regex_ast(RegexFlags {
            case_insensitive: false,
            multiline: false,
            dot_all: true,
        }));
        let with_all = serialize_query(&build_regex_ast(RegexFlags {
            case_insensitive: true,
            multiline: true,
            dot_all: true,
        }));

        assert_ne!(
            no_flags, with_i,
            "Cache collision: no flags vs case_insensitive"
        );
        assert_ne!(no_flags, with_m, "Cache collision: no flags vs multiline");
        assert_ne!(no_flags, with_s, "Cache collision: no flags vs dot_all");
        assert_ne!(
            with_i, with_m,
            "Cache collision: case_insensitive vs multiline"
        );
        assert_ne!(
            with_i, with_all,
            "Cache collision: case_insensitive vs all flags"
        );
    }

    // D3 Advanced Query Feature tests: serialization

    #[test]
    fn test_serialize_variable() {
        let ast = QueryAST {
            root: Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::Variable("type".to_string()),
                span: Span::new(0, 10),
            }),
            span: Span::new(0, 10),
        };
        let serialized = serialize_query(&ast);
        assert_eq!(serialized, "kind:$type");
    }

    #[test]
    fn test_serialize_join() {
        use crate::query::types::JoinEdgeKind;

        let ast = QueryAST {
            root: Expr::Join(crate::query::types::JoinExpr {
                left: Box::new(Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(0, 13),
                })),
                edge: JoinEdgeKind::Calls,
                right: Box::new(Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::new(20, 33),
                })),
                span: Span::new(0, 33),
            }),
            span: Span::new(0, 33),
        };
        let serialized = serialize_query(&ast);
        assert_eq!(serialized, "(kind:function) CALLS (kind:function)");
    }
}
