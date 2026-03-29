//! Core types for the query language
//!
//! This module defines the Abstract Syntax Tree (AST) types, operators, values,
//! and field descriptors used in sqry's query language.

use std::fmt;

/// A complete query with its root expression and span information
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// Root expression of the query
    pub root: Expr,
    /// Span covering the entire query
    pub span: Span,
}

impl Query {
    /// Check if this query contains any relation predicates (Sprint 2)
    #[must_use]
    pub fn has_relation_predicates(&self) -> bool {
        Self::expr_has_relation_predicates(&self.root)
    }

    /// Check if this query contains any scope predicates (P2-34 Phase 2)
    ///
    /// Scope predicates (scope.type, scope.name, scope.parent, scope.ancestor) require
    /// access to graph file scopes for evaluation.
    #[must_use]
    pub fn has_scope_predicates(&self) -> bool {
        Self::expr_has_scope_predicates(&self.root)
    }

    /// Check if this query contains reference predicates (P2-33)
    ///
    /// Reference predicates (references:) require access to graph reference edges.
    #[must_use]
    pub fn has_reference_predicates(&self) -> bool {
        Self::expr_has_reference_predicates(&self.root)
    }

    /// Check if this query contains CD (Cross-file Discovery) predicates
    ///
    /// CD predicates (duplicates:, unused:, circular:) require access to the unified
    /// graph for cross-node analysis (duplicate detection, dead code, cycle detection).
    #[must_use]
    pub fn has_cd_predicates(&self) -> bool {
        Self::expr_has_cd_predicates(&self.root)
    }

    /// Recursively check if an expression contains relation predicates
    fn expr_has_relation_predicates(expr: &Expr) -> bool {
        match expr {
            Expr::And(operands) | Expr::Or(operands) => {
                operands.iter().any(Self::expr_has_relation_predicates)
            }
            Expr::Not(operand) => Self::expr_has_relation_predicates(operand),
            Expr::Condition(condition) => {
                // callers/callees/imports/exports/impl require graph edges
                // returns: is a metadata predicate (checks symbol.metadata["return_type"])
                matches!(
                    condition.field.as_str(),
                    "callers" | "callees" | "imports" | "exports" | "impl"
                )
            }
            Expr::Join(_) => true, // Joins always use relation edges
        }
    }

    /// Recursively check if an expression contains scope predicates (P2-34)
    fn expr_has_scope_predicates(expr: &Expr) -> bool {
        match expr {
            Expr::And(operands) | Expr::Or(operands) => {
                operands.iter().any(Self::expr_has_scope_predicates)
            }
            Expr::Not(operand) => Self::expr_has_scope_predicates(operand),
            Expr::Condition(condition) => {
                // scope.type, scope.name, scope.parent, scope.ancestor require graph scopes
                matches!(
                    condition.field.as_str(),
                    "scope.type" | "scope.name" | "scope.parent" | "scope.ancestor"
                )
            }
            Expr::Join(join) => {
                Self::expr_has_scope_predicates(&join.left)
                    || Self::expr_has_scope_predicates(&join.right)
            }
        }
    }

    /// Recursively check if an expression contains reference predicates (P2-33)
    fn expr_has_reference_predicates(expr: &Expr) -> bool {
        match expr {
            Expr::And(operands) | Expr::Or(operands) => {
                operands.iter().any(Self::expr_has_reference_predicates)
            }
            Expr::Not(operand) => Self::expr_has_reference_predicates(operand),
            Expr::Condition(condition) => {
                // references: requires graph edges
                matches!(condition.field.as_str(), "references")
            }
            Expr::Join(join) => {
                Self::expr_has_reference_predicates(&join.left)
                    || Self::expr_has_reference_predicates(&join.right)
            }
        }
    }

    /// Recursively check if an expression contains CD predicates
    fn expr_has_cd_predicates(expr: &Expr) -> bool {
        match expr {
            Expr::And(operands) | Expr::Or(operands) => {
                operands.iter().any(Self::expr_has_cd_predicates)
            }
            Expr::Not(operand) => Self::expr_has_cd_predicates(operand),
            Expr::Condition(condition) => {
                // CD predicates require graph-wide analysis
                matches!(
                    condition.field.as_str(),
                    "duplicates" | "unused" | "circular"
                )
            }
            Expr::Join(join) => {
                Self::expr_has_cd_predicates(&join.left)
                    || Self::expr_has_cd_predicates(&join.right)
            }
        }
    }
}

/// An expression in the query AST
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Boolean OR: at least one operand must match
    Or(Vec<Expr>),

    /// Boolean AND: all operands must match
    And(Vec<Expr>),

    /// Boolean NOT: operand must not match
    Not(Box<Expr>),

    /// Condition: field operator value
    Condition(Condition),

    /// Join: two expressions connected by an edge type
    Join(JoinExpr),
}

/// Edge kind for join queries
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinEdgeKind {
    /// Function calls
    Calls,
    /// Import relationships
    Imports,
    /// Inheritance relationships
    Inherits,
    /// Interface implementation
    Implements,
}

impl fmt::Display for JoinEdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JoinEdgeKind::Calls => write!(f, "CALLS"),
            JoinEdgeKind::Imports => write!(f, "IMPORTS"),
            JoinEdgeKind::Inherits => write!(f, "INHERITS"),
            JoinEdgeKind::Implements => write!(f, "IMPLEMENTS"),
        }
    }
}

/// A join expression connecting two queries by an edge type
#[derive(Debug, Clone, PartialEq)]
pub struct JoinExpr {
    /// Left-hand side query
    pub left: Box<Expr>,
    /// Type of edge connecting left to right
    pub edge: JoinEdgeKind,
    /// Right-hand side query
    pub right: Box<Expr>,
    /// Source span for error reporting
    pub span: Span,
}

/// A condition comparing a field to a value using an operator
#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    /// Field name (e.g., "kind", "name", "async")
    pub field: Field,
    /// Comparison operator
    pub operator: Operator,
    /// Value to compare against
    pub value: Value,
    /// Source span for error reporting
    pub span: Span,
}

/// A field name in a query
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field(pub String);

impl Field {
    /// Create a new field
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// Get the field name as a string slice
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Operators supported in conditions
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operator {
    /// `:` operator - semantics depend on field type:
    /// - String/Enum fields: exact match (`name:foo` matches "foo" exactly)
    /// - Path fields: glob match (`path:src/**/*.rs` uses glob syntax)
    /// - Boolean/Number fields: equality check
    Equal,

    /// `~=` operator - regex match
    /// Compiles pattern with flags (case-insensitive, multiline, dot-all)
    Regex,

    /// `>` - greater than (numeric fields only)
    Greater,

    /// `<` - less than (numeric fields only)
    Less,

    /// `>=` - greater than or equal (numeric fields only)
    GreaterEq,

    /// `<=` - less than or equal (numeric fields only)
    LessEq,
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operator::Equal => write!(f, ":"),
            Operator::Regex => write!(f, "~="),
            Operator::Greater => write!(f, ">"),
            Operator::Less => write!(f, "<"),
            Operator::GreaterEq => write!(f, ">="),
            Operator::LessEq => write!(f, "<="),
        }
    }
}

/// Values in conditions
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// String value (quoted or unquoted)
    String(String),

    /// Regex pattern with flags
    Regex(RegexValue),

    /// Numeric value
    Number(i64),

    /// Boolean value
    Boolean(bool),

    /// Variable reference (e.g., `$type` in `kind:$type`)
    Variable(std::string::String),

    /// Subquery expression (e.g., `callers:(kind:function AND async:true)`)
    Subquery(Box<Expr>),
}

impl Value {
    /// Extract string value if this is a String variant
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Extract regex value if this is a Regex variant
    #[must_use]
    pub fn as_regex(&self) -> Option<&RegexValue> {
        match self {
            Value::Regex(r) => Some(r),
            _ => None,
        }
    }

    /// Extract number value if this is a Number variant
    #[must_use]
    pub fn as_number(&self) -> Option<i64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Extract boolean value if this is a Boolean variant
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Extract variable name if this is a Variable variant
    #[must_use]
    pub fn as_variable(&self) -> Option<&str> {
        match self {
            Value::Variable(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// Get the type name of this value as a string
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Regex(_) => "regex",
            Value::Number(_) => "number",
            Value::Boolean(_) => "boolean",
            Value::Variable(_) => "variable",
            Value::Subquery(_) => "subquery",
        }
    }
}

/// A regex pattern with compilation flags
#[derive(Debug, Clone, PartialEq)]
pub struct RegexValue {
    /// The regex pattern string
    pub pattern: String,
    /// Compilation flags
    pub flags: RegexFlags,
}

/// Regex compilation flags
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RegexFlags {
    /// Case-insensitive matching (flag: i)
    pub case_insensitive: bool,
    /// Multiline mode - ^ and $ match line boundaries (flag: m)
    pub multiline: bool,
    /// Dot matches newlines (flag: s)
    pub dot_all: bool,
}

/// Position span for error reporting
///
/// Tracks the location of a token or expression in the source query string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the start position in the source
    pub start: usize,
    /// Byte offset of the end position in the source
    pub end: usize,
    /// Line number (1-indexed)
    pub line: usize,
    /// Column number (1-indexed)
    pub column: usize,
}

impl Span {
    /// Create a new span
    #[must_use]
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start,
            end,
            line: 1,
            column: 1,
        }
    }

    /// Create a synthetic span for programmatically built queries.
    ///
    /// Uses 1-based line/column (matching parsed query conventions) but
    /// start=0, end=0 to indicate no source text.
    ///
    /// # Example
    ///
    /// ```
    /// # use sqry_core::query::types::Span;
    /// let span = Span::synthetic();
    /// assert!(span.is_synthetic());
    /// assert_eq!(span.line, 1);
    /// assert_eq!(span.column, 1);
    /// ```
    #[must_use]
    pub fn synthetic() -> Self {
        Self {
            start: 0,
            end: 0,
            line: 1,
            column: 1,
        }
    }

    /// Check if this span is synthetic (no source location).
    ///
    /// A synthetic span is created by the Query Builder API for programmatically
    /// constructed queries that don't have a corresponding source string.
    ///
    /// Error formatters can use this to omit location information when
    /// displaying errors from builder-constructed queries.
    ///
    /// # Example
    ///
    /// ```
    /// # use sqry_core::query::types::Span;
    /// let synthetic = Span::synthetic();
    /// assert!(synthetic.is_synthetic());
    ///
    /// let parsed = Span::new(5, 10);
    /// assert!(!parsed.is_synthetic());
    /// ```
    #[must_use]
    pub fn is_synthetic(&self) -> bool {
        self.start == 0 && self.end == 0
    }

    /// Create a span with line and column information
    #[must_use]
    pub fn with_position(start: usize, end: usize, line: usize, column: usize) -> Self {
        Self {
            start,
            end,
            line,
            column,
        }
    }

    /// Merge two spans, using the earliest start and latest end
    ///
    /// # Example
    ///
    /// ```
    /// # use sqry_core::query::types::Span;
    /// let span1 = Span::new(0, 5);
    /// let span2 = Span::new(7, 12);
    /// let merged = span1.merge(&span2);
    /// assert_eq!(merged.start, 0);
    /// assert_eq!(merged.end, 12);
    /// ```
    #[must_use]
    pub fn merge(&self, other: &Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
            line: self.line.min(other.line),
            column: self.column, // Use start column
        }
    }
}

impl Default for Span {
    /// Creates a default span at position 0
    ///
    /// # Example
    ///
    /// ```
    /// # use sqry_core::query::types::Span;
    /// let span = Span::default();
    /// assert_eq!(span.start, 0);
    /// assert_eq!(span.end, 0);
    /// assert_eq!(span.line, 1);
    /// assert_eq!(span.column, 1);
    /// ```
    fn default() -> Self {
        Self {
            start: 0,
            end: 0,
            line: 1,
            column: 1,
        }
    }
}

/// Field type for validation
#[derive(Debug, Clone, PartialEq)]
pub enum FieldType {
    /// String field
    String,
    /// Boolean field
    Bool,
    /// Numeric field
    Number,
    /// Enumeration with allowed values
    Enum(Vec<&'static str>),
    /// File path field (supports glob patterns)
    Path,
}

/// Field descriptor for query validation
///
/// Describes the properties of a queryable field, including its type,
/// supported operators, indexing status, and documentation.
#[derive(Debug, Clone)]
pub struct FieldDescriptor {
    /// Field name
    pub name: &'static str,

    /// Field type
    pub field_type: FieldType,

    /// Operators supported for this field
    pub operators: &'static [Operator],

    /// Whether this field is indexed (for query optimization)
    pub indexed: bool,

    /// Documentation string
    pub doc: &'static str,
}

impl FieldDescriptor {
    /// Check if this field supports the given operator
    #[inline]
    #[must_use]
    pub fn supports_operator(&self, operator: &Operator) -> bool {
        self.operators.contains(operator)
    }

    /// Check if the value type matches this field's type
    #[must_use]
    pub fn matches_value_type(&self, value: &Value) -> bool {
        match value {
            // Variables and subqueries are validated after resolution
            Value::Variable(_) | Value::Subquery(_) => true,
            _ => matches!(
                (&self.field_type, value),
                (
                    FieldType::String | FieldType::Enum(_) | FieldType::Path,
                    Value::String(_)
                ) | (FieldType::Bool, Value::Boolean(_))
                    | (FieldType::Number, Value::Number(_))
            ),
        }
    }
}

/// A complete pipeline query: base query + aggregation stages
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineQuery {
    /// The base query to execute first
    pub query: Query,
    /// Aggregation stages applied in order
    pub stages: Vec<PipelineStage>,
    /// Span covering the entire pipeline query
    pub span: Span,
}

/// An aggregation stage in a pipeline query
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineStage {
    /// Count total matches
    Count,
    /// Group by a field
    GroupBy {
        /// Field to group by
        field: Field,
    },
    /// Top N entries by a field
    Top {
        /// Number of entries to return
        n: usize,
        /// Field to rank by
        field: Field,
    },
    /// Comprehensive statistics
    Stats,
}

/// Check if a field name is a relation field (used for subquery parsing).
#[must_use]
pub fn is_relation_field(field: &str) -> bool {
    matches!(
        field,
        "callers" | "callees" | "imports" | "exports" | "impl" | "implements" | "references"
    )
}

/// Maximum recursion depth for subquery nesting.
pub const MAX_SUBQUERY_DEPTH: usize = 4;

/// Resolve variables in an expression tree by substituting `Value::Variable(name)`
/// with the looked-up value from the provided map.
///
/// The substituted strings are parsed into appropriate `Value` variants:
/// - `"true"` / `"false"` → `Value::Boolean`
/// - Numeric strings → `Value::Number`
/// - Everything else → `Value::String`
///
/// # Errors
///
/// Returns an error if a variable is referenced but not in the provided map.
pub fn resolve_variables<S: std::hash::BuildHasher>(
    expr: &Expr,
    variables: &std::collections::HashMap<String, String, S>,
) -> Result<Expr, String> {
    match expr {
        Expr::And(operands) => {
            let resolved: Result<Vec<_>, _> = operands
                .iter()
                .map(|op| resolve_variables(op, variables))
                .collect();
            Ok(Expr::And(resolved?))
        }
        Expr::Or(operands) => {
            let resolved: Result<Vec<_>, _> = operands
                .iter()
                .map(|op| resolve_variables(op, variables))
                .collect();
            Ok(Expr::Or(resolved?))
        }
        Expr::Not(operand) => Ok(Expr::Not(Box::new(resolve_variables(operand, variables)?))),
        Expr::Condition(condition) => {
            let resolved_value = match &condition.value {
                Value::Variable(name) => {
                    let raw = variables
                        .get(name.as_str())
                        .ok_or_else(|| format!("Unresolved variable: ${name}"))?;
                    parse_variable_value(raw)
                }
                Value::Subquery(inner) => {
                    Value::Subquery(Box::new(resolve_variables(inner, variables)?))
                }
                other => other.clone(),
            };
            Ok(Expr::Condition(Condition {
                field: condition.field.clone(),
                operator: condition.operator.clone(),
                value: resolved_value,
                span: condition.span.clone(),
            }))
        }
        Expr::Join(join) => Ok(Expr::Join(JoinExpr {
            left: Box::new(resolve_variables(&join.left, variables)?),
            edge: join.edge.clone(),
            right: Box::new(resolve_variables(&join.right, variables)?),
            span: join.span.clone(),
        })),
    }
}

/// Parse a raw string variable value into the appropriate `Value` variant.
fn parse_variable_value(raw: &str) -> Value {
    match raw.to_lowercase().as_str() {
        "true" => Value::Boolean(true),
        "false" => Value::Boolean(false),
        _ => {
            if let Ok(n) = raw.parse::<i64>() {
                Value::Number(n)
            } else {
                Value::String(raw.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_default() {
        let span = Span::default();
        assert_eq!(span.start, 0);
        assert_eq!(span.end, 0);
        assert_eq!(span.line, 1);
        assert_eq!(span.column, 1);
    }

    #[test]
    fn test_span_synthetic() {
        let span = Span::synthetic();
        assert_eq!(span.start, 0);
        assert_eq!(span.end, 0);
        assert_eq!(span.line, 1);
        assert_eq!(span.column, 1);
        assert!(span.is_synthetic());
    }

    #[test]
    fn test_span_is_synthetic() {
        // Synthetic span
        let synthetic = Span::synthetic();
        assert!(synthetic.is_synthetic());

        // Parsed span with position
        let parsed = Span::new(5, 10);
        assert!(!parsed.is_synthetic());

        // Edge case: start=0 but end!=0 is not synthetic
        let partial = Span::new(0, 5);
        assert!(!partial.is_synthetic());
    }

    #[test]
    fn test_span_merge() {
        let span1 = Span::new(0, 5);
        let span2 = Span::new(7, 12);
        let merged = span1.merge(&span2);
        assert_eq!(merged.start, 0);
        assert_eq!(merged.end, 12);
    }

    #[test]
    fn test_span_merge_overlapping() {
        let span1 = Span::new(0, 10);
        let span2 = Span::new(5, 15);
        let merged = span1.merge(&span2);
        assert_eq!(merged.start, 0);
        assert_eq!(merged.end, 15);
    }

    #[test]
    fn test_span_merge_reverse_order() {
        let span1 = Span::new(10, 15);
        let span2 = Span::new(0, 5);
        let merged = span1.merge(&span2);
        assert_eq!(merged.start, 0);
        assert_eq!(merged.end, 15);
    }

    #[test]
    fn test_span_with_position() {
        let span = Span::with_position(10, 20, 2, 5);
        assert_eq!(span.start, 10);
        assert_eq!(span.end, 20);
        assert_eq!(span.line, 2);
        assert_eq!(span.column, 5);
    }

    #[test]
    fn test_operator_display() {
        assert_eq!(Operator::Equal.to_string(), ":");
        assert_eq!(Operator::Regex.to_string(), "~=");
        assert_eq!(Operator::Greater.to_string(), ">");
        assert_eq!(Operator::Less.to_string(), "<");
        assert_eq!(Operator::GreaterEq.to_string(), ">=");
        assert_eq!(Operator::LessEq.to_string(), "<=");
    }

    #[test]
    fn test_field_creation() {
        let field = Field::new("kind");
        assert_eq!(field.as_str(), "kind");
        assert_eq!(field.to_string(), "kind");
    }

    #[test]
    fn test_value_constructors() {
        let str_val = Value::String("hello".to_string());
        assert!(matches!(str_val, Value::String(_)));

        let num_val = Value::Number(42);
        assert!(matches!(num_val, Value::Number(42)));

        let bool_val = Value::Boolean(true);
        assert!(matches!(bool_val, Value::Boolean(true)));

        let regex_val = Value::Regex(RegexValue {
            pattern: "^test".to_string(),
            flags: RegexFlags::default(),
        });
        assert!(matches!(regex_val, Value::Regex(_)));
    }

    #[test]
    fn test_regex_flags_default() {
        let flags = RegexFlags::default();
        assert!(!flags.case_insensitive);
        assert!(!flags.multiline);
        assert!(!flags.dot_all);
    }

    #[test]
    fn test_field_descriptor_supports_operator() {
        let descriptor = FieldDescriptor {
            name: "kind",
            field_type: FieldType::Enum(vec!["function", "class"]),
            operators: &[Operator::Equal, Operator::Regex],
            indexed: true,
            doc: "Node type",
        };

        assert!(descriptor.supports_operator(&Operator::Equal));
        assert!(descriptor.supports_operator(&Operator::Regex));
        assert!(!descriptor.supports_operator(&Operator::Greater));
    }

    #[test]
    fn test_field_descriptor_matches_value_type() {
        let string_desc = FieldDescriptor {
            name: "name",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: true,
            doc: "Name field",
        };

        assert!(string_desc.matches_value_type(&Value::String("test".to_string())));
        assert!(!string_desc.matches_value_type(&Value::Number(42)));

        let bool_desc = FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Async field",
        };

        assert!(bool_desc.matches_value_type(&Value::Boolean(true)));
        assert!(!bool_desc.matches_value_type(&Value::String("true".to_string())));
    }

    // D3 Advanced Query Feature tests: variable resolution

    #[test]
    fn test_resolve_variables_simple() {
        let expr = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::Variable("type".to_string()),
            span: Span::synthetic(),
        });
        let mut vars = std::collections::HashMap::new();
        vars.insert("type".to_string(), "function".to_string());
        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Condition(cond) => {
                assert_eq!(cond.value, Value::String("function".to_string()));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_resolve_variables_missing_error() {
        let expr = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::Variable("missing".to_string()),
            span: Span::synthetic(),
        });
        let vars = std::collections::HashMap::new();
        let result = resolve_variables(&expr, &vars);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .contains("Unresolved variable: $missing")
        );
    }

    #[test]
    fn test_resolve_variables_boolean_coercion() {
        let expr = Expr::Condition(Condition {
            field: Field::new("async"),
            operator: Operator::Equal,
            value: Value::Variable("flag".to_string()),
            span: Span::synthetic(),
        });
        let mut vars = std::collections::HashMap::new();
        vars.insert("flag".to_string(), "true".to_string());
        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Condition(cond) => {
                assert_eq!(cond.value, Value::Boolean(true));
            }
            _ => panic!("Expected Condition"),
        }
    }

    #[test]
    fn test_resolve_variables_number_coercion() {
        let expr = Expr::Condition(Condition {
            field: Field::new("lines"),
            operator: Operator::Greater,
            value: Value::Variable("count".to_string()),
            span: Span::synthetic(),
        });
        let mut vars = std::collections::HashMap::new();
        vars.insert("count".to_string(), "42".to_string());
        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Condition(cond) => {
                assert_eq!(cond.value, Value::Number(42));
            }
            _ => panic!("Expected Condition"),
        }
    }

    // ── Value accessor method tests ────────────────────────────────────────

    #[test]
    fn test_value_as_string_some() {
        let val = Value::String("hello".to_string());
        assert_eq!(val.as_string(), Some("hello"));
    }

    #[test]
    fn test_value_as_string_none_for_other_variants() {
        assert!(Value::Number(42).as_string().is_none());
        assert!(Value::Boolean(true).as_string().is_none());
    }

    #[test]
    fn test_value_as_regex_some() {
        let regex = RegexValue {
            pattern: "foo.*".to_string(),
            flags: RegexFlags::default(),
        };
        let val = Value::Regex(regex.clone());
        let extracted = val.as_regex().unwrap();
        assert_eq!(extracted.pattern, "foo.*");
    }

    #[test]
    fn test_value_as_regex_none_for_other_variants() {
        assert!(Value::String("x".to_string()).as_regex().is_none());
        assert!(Value::Number(1).as_regex().is_none());
    }

    #[test]
    fn test_value_as_number_some() {
        let val = Value::Number(-7);
        assert_eq!(val.as_number(), Some(-7));
    }

    #[test]
    fn test_value_as_number_none_for_other_variants() {
        assert!(Value::String("7".to_string()).as_number().is_none());
        assert!(Value::Boolean(false).as_number().is_none());
    }

    #[test]
    fn test_value_as_bool_some() {
        assert_eq!(Value::Boolean(true).as_bool(), Some(true));
        assert_eq!(Value::Boolean(false).as_bool(), Some(false));
    }

    #[test]
    fn test_value_as_bool_none_for_other_variants() {
        assert!(Value::String("true".to_string()).as_bool().is_none());
        assert!(Value::Number(1).as_bool().is_none());
    }

    #[test]
    fn test_value_as_variable_some() {
        let val = Value::Variable("my_var".to_string());
        assert_eq!(val.as_variable(), Some("my_var"));
    }

    #[test]
    fn test_value_as_variable_none_for_other_variants() {
        assert!(Value::String("x".to_string()).as_variable().is_none());
        assert!(Value::Number(0).as_variable().is_none());
    }

    #[test]
    fn test_value_type_name_all_variants() {
        assert_eq!(Value::String("s".to_string()).type_name(), "string");
        assert_eq!(
            Value::Regex(RegexValue {
                pattern: "x".to_string(),
                flags: RegexFlags::default()
            })
            .type_name(),
            "regex"
        );
        assert_eq!(Value::Number(0).type_name(), "number");
        assert_eq!(Value::Boolean(true).type_name(), "boolean");
        assert_eq!(Value::Variable("v".to_string()).type_name(), "variable");
        assert_eq!(
            Value::Subquery(Box::new(Expr::And(vec![]))).type_name(),
            "subquery"
        );
    }

    // ── JoinEdgeKind Display tests ─────────────────────────────────────────

    #[test]
    fn test_join_edge_kind_display_all_variants() {
        assert_eq!(JoinEdgeKind::Calls.to_string(), "CALLS");
        assert_eq!(JoinEdgeKind::Imports.to_string(), "IMPORTS");
        assert_eq!(JoinEdgeKind::Inherits.to_string(), "INHERITS");
        assert_eq!(JoinEdgeKind::Implements.to_string(), "IMPLEMENTS");
    }

    // ── is_relation_field tests ────────────────────────────────────────────

    #[test]
    fn test_is_relation_field_true_values() {
        for field in &[
            "callers",
            "callees",
            "imports",
            "exports",
            "impl",
            "implements",
            "references",
        ] {
            assert!(
                is_relation_field(field),
                "expected {field} to be a relation field"
            );
        }
    }

    #[test]
    fn test_is_relation_field_false_values() {
        assert!(!is_relation_field("kind"));
        assert!(!is_relation_field("name"));
        assert!(!is_relation_field("async"));
        assert!(!is_relation_field(""));
    }

    // ── Query predicate checker tests ─────────────────────────────────────

    fn make_query(root: Expr) -> Query {
        Query {
            root,
            span: Span::synthetic(),
        }
    }

    fn cond(field: &str) -> Expr {
        Expr::Condition(Condition {
            field: Field::new(field),
            operator: Operator::Equal,
            value: Value::Boolean(true),
            span: Span::synthetic(),
        })
    }

    #[test]
    fn test_has_relation_predicates_true_for_callers() {
        let q = make_query(cond("callers"));
        assert!(q.has_relation_predicates());
    }

    #[test]
    fn test_has_relation_predicates_false_for_kind() {
        let q = make_query(cond("kind"));
        assert!(!q.has_relation_predicates());
    }

    #[test]
    fn test_has_relation_predicates_via_and_or_not() {
        // AND containing a relation field
        let q = make_query(Expr::And(vec![cond("name"), cond("callees")]));
        assert!(q.has_relation_predicates());

        // OR containing a relation field
        let q = make_query(Expr::Or(vec![cond("kind"), cond("exports")]));
        assert!(q.has_relation_predicates());

        // NOT wrapping a relation field
        let q = make_query(Expr::Not(Box::new(cond("imports"))));
        assert!(q.has_relation_predicates());
    }

    #[test]
    fn test_has_relation_predicates_join_always_true() {
        let q = make_query(Expr::Join(JoinExpr {
            left: Box::new(cond("name")),
            edge: JoinEdgeKind::Calls,
            right: Box::new(cond("kind")),
            span: Span::synthetic(),
        }));
        assert!(q.has_relation_predicates());
    }

    #[test]
    fn test_has_scope_predicates_true_and_false() {
        let q = make_query(cond("scope.type"));
        assert!(q.has_scope_predicates());

        let q = make_query(cond("name"));
        assert!(!q.has_scope_predicates());
    }

    #[test]
    fn test_has_scope_predicates_via_join() {
        let q = make_query(Expr::Join(JoinExpr {
            left: Box::new(cond("scope.name")),
            edge: JoinEdgeKind::Calls,
            right: Box::new(cond("kind")),
            span: Span::synthetic(),
        }));
        assert!(q.has_scope_predicates());
    }

    #[test]
    fn test_has_reference_predicates_true_and_false() {
        let q = make_query(cond("references"));
        assert!(q.has_reference_predicates());

        let q = make_query(cond("kind"));
        assert!(!q.has_reference_predicates());
    }

    #[test]
    fn test_has_reference_predicates_via_join() {
        let q = make_query(Expr::Join(JoinExpr {
            left: Box::new(cond("name")),
            edge: JoinEdgeKind::Imports,
            right: Box::new(cond("references")),
            span: Span::synthetic(),
        }));
        assert!(q.has_reference_predicates());
    }

    #[test]
    fn test_has_cd_predicates_true_and_false() {
        let q = make_query(cond("duplicates"));
        assert!(q.has_cd_predicates());

        let q = make_query(cond("unused"));
        assert!(q.has_cd_predicates());

        let q = make_query(cond("circular"));
        assert!(q.has_cd_predicates());

        let q = make_query(cond("name"));
        assert!(!q.has_cd_predicates());
    }

    #[test]
    fn test_has_cd_predicates_via_join() {
        let q = make_query(Expr::Join(JoinExpr {
            left: Box::new(cond("unused")),
            edge: JoinEdgeKind::Inherits,
            right: Box::new(cond("kind")),
            span: Span::synthetic(),
        }));
        assert!(q.has_cd_predicates());
    }

    // ── resolve_variables for And, Or, Not, Join, Subquery ────────────────

    #[test]
    fn test_resolve_variables_and_expr() {
        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::Variable("t".to_string()),
                span: Span::synthetic(),
            }),
            Expr::Condition(Condition {
                field: Field::new("async"),
                operator: Operator::Equal,
                value: Value::Variable("a".to_string()),
                span: Span::synthetic(),
            }),
        ]);
        let mut vars = std::collections::HashMap::new();
        vars.insert("t".to_string(), "function".to_string());
        vars.insert("a".to_string(), "true".to_string());

        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);
                match &operands[0] {
                    Expr::Condition(c) => {
                        assert_eq!(c.value, Value::String("function".to_string()))
                    }
                    _ => panic!("expected Condition"),
                }
                match &operands[1] {
                    Expr::Condition(c) => assert_eq!(c.value, Value::Boolean(true)),
                    _ => panic!("expected Condition"),
                }
            }
            _ => panic!("expected And"),
        }
    }

    #[test]
    fn test_resolve_variables_or_expr() {
        let expr = Expr::Or(vec![Expr::Condition(Condition {
            field: Field::new("name"),
            operator: Operator::Equal,
            value: Value::Variable("n".to_string()),
            span: Span::synthetic(),
        })]);
        let mut vars = std::collections::HashMap::new();
        vars.insert("n".to_string(), "foo".to_string());

        let resolved = resolve_variables(&expr, &vars).unwrap();
        assert!(matches!(resolved, Expr::Or(_)));
    }

    #[test]
    fn test_resolve_variables_not_expr() {
        let expr = Expr::Not(Box::new(Expr::Condition(Condition {
            field: Field::new("async"),
            operator: Operator::Equal,
            value: Value::Variable("flag".to_string()),
            span: Span::synthetic(),
        })));
        let mut vars = std::collections::HashMap::new();
        vars.insert("flag".to_string(), "false".to_string());

        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Not(inner) => match *inner {
                Expr::Condition(c) => assert_eq!(c.value, Value::Boolean(false)),
                _ => panic!("expected Condition inside Not"),
            },
            _ => panic!("expected Not"),
        }
    }

    #[test]
    fn test_resolve_variables_join_expr() {
        let expr = Expr::Join(JoinExpr {
            left: Box::new(Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::Variable("lkind".to_string()),
                span: Span::synthetic(),
            })),
            edge: JoinEdgeKind::Calls,
            right: Box::new(Expr::Condition(Condition {
                field: Field::new("name"),
                operator: Operator::Equal,
                value: Value::String("bar".to_string()),
                span: Span::synthetic(),
            })),
            span: Span::synthetic(),
        });
        let mut vars = std::collections::HashMap::new();
        vars.insert("lkind".to_string(), "function".to_string());

        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Join(j) => match *j.left {
                Expr::Condition(c) => assert_eq!(c.value, Value::String("function".to_string())),
                _ => panic!("expected Condition"),
            },
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn test_resolve_variables_subquery_value() {
        // Value::Subquery inside a Condition should be recursively resolved
        let inner_cond = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::Variable("inner_var".to_string()),
            span: Span::synthetic(),
        });
        let expr = Expr::Condition(Condition {
            field: Field::new("callers"),
            operator: Operator::Equal,
            value: Value::Subquery(Box::new(inner_cond)),
            span: Span::synthetic(),
        });

        let mut vars = std::collections::HashMap::new();
        vars.insert("inner_var".to_string(), "method".to_string());

        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Condition(c) => match c.value {
                Value::Subquery(inner) => match *inner {
                    Expr::Condition(ic) => {
                        assert_eq!(ic.value, Value::String("method".to_string()));
                    }
                    _ => panic!("expected inner Condition"),
                },
                _ => panic!("expected Subquery value"),
            },
            _ => panic!("expected Condition"),
        }
    }

    #[test]
    fn test_resolve_variables_non_variable_passthrough() {
        // Non-variable values should be cloned through unchanged
        let expr = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::String("function".to_string()),
            span: Span::synthetic(),
        });
        let vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let resolved = resolve_variables(&expr, &vars).unwrap();
        match resolved {
            Expr::Condition(c) => assert_eq!(c.value, Value::String("function".to_string())),
            _ => panic!("expected Condition"),
        }
    }
}
