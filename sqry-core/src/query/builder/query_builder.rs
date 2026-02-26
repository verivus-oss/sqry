//! Main [`QueryBuilder`] implementation

use std::sync::Arc;

use super::{BuildError, ConditionBuilder, RegexBuilder};
use crate::query::registry::FieldRegistry;
use crate::query::types::{
    Expr, FieldType, Operator, Query as QueryAST, RegexFlags, RegexValue, Span, Value,
};

/// Builder for constructing type-safe queries.
///
/// # Example
///
/// ```ignore
/// use sqry_core::query::builder::QueryBuilder;
///
/// let query = QueryBuilder::kind("function")
///     .and(QueryBuilder::lang("rust"))
///     .and_not(QueryBuilder::name_matches("test.*"))
///     .build()?;
/// ```
#[derive(Clone, Debug)]
#[must_use = "QueryBuilder does nothing until .build() is called"]
pub struct QueryBuilder {
    /// The expression being built
    expr: BuilderExpr,
    /// Accumulated validation errors (lazy validation)
    errors: Vec<BuildError>,
}

/// Internal expression representation during building
#[derive(Clone, Debug)]
enum BuilderExpr {
    /// Single condition
    Condition(ConditionBuilder),
    /// AND of multiple expressions
    And(Vec<QueryBuilder>),
    /// OR of multiple expressions
    Or(Vec<QueryBuilder>),
    /// Negation of expression
    Not(Box<QueryBuilder>),
    /// Empty builder (for chaining from `new()`)
    Empty,
}

// ============================================================================
// Constructor Methods
// ============================================================================

impl QueryBuilder {
    /// Create empty builder for chaining
    pub fn new() -> Self {
        Self {
            expr: BuilderExpr::Empty,
            errors: Vec::new(),
        }
    }

    // ========================================================================
    // Node Identity Fields
    // ========================================================================

    /// Filter by symbol kind (function, method, class, etc.)
    pub fn kind(value: impl Into<String>) -> Self {
        Self::condition("kind", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by multiple symbol kinds (OR)
    pub fn kind_any(values: &[&str]) -> Self {
        Self::any(values.iter().map(|v| Self::kind(*v)).collect())
    }

    /// Filter by symbol name (exact match)
    pub fn name(value: impl Into<String>) -> Self {
        Self::condition("name", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by symbol name (regex match with default flags)
    pub fn name_matches(pattern: impl Into<String>) -> Self {
        let regex = RegexValue {
            pattern: pattern.into(),
            flags: RegexFlags::default(),
        };
        Self::condition("name", Operator::Regex, Value::Regex(regex))
    }

    /// Filter by symbol name (regex match with custom flags via closure)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Case-insensitive name matching
    /// QueryBuilder::name_matches_with("test.*", |rb| rb.case_insensitive())
    /// ```
    pub fn name_matches_with<F>(pattern: impl Into<String>, configure: F) -> Self
    where
        F: FnOnce(RegexBuilder) -> RegexBuilder,
    {
        let builder = RegexBuilder::new(pattern);
        let configured = configure(builder);
        Self::condition(
            "name",
            Operator::Regex,
            Value::Regex(configured.into_regex_value()),
        )
    }

    /// Filter by programming language
    pub fn lang(value: impl Into<String>) -> Self {
        Self::condition("lang", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by programming language (alias for lang)
    pub fn language(value: impl Into<String>) -> Self {
        Self::lang(value)
    }

    // ========================================================================
    // Location Fields
    // ========================================================================

    /// Filter by file path (exact or glob match)
    pub fn path(value: impl Into<String>) -> Self {
        Self::condition("path", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by file path (alias for path)
    pub fn file(value: impl Into<String>) -> Self {
        Self::path(value)
    }

    /// Filter by file path (regex match with default flags)
    pub fn path_matches(pattern: impl Into<String>) -> Self {
        let regex = RegexValue {
            pattern: pattern.into(),
            flags: RegexFlags::default(),
        };
        Self::condition("path", Operator::Regex, Value::Regex(regex))
    }

    /// Filter by file path (regex match with custom flags via closure)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Case-insensitive path matching
    /// QueryBuilder::path_matches_with(".*test.*", |rb| rb.case_insensitive())
    /// ```
    pub fn path_matches_with<F>(pattern: impl Into<String>, configure: F) -> Self
    where
        F: FnOnce(RegexBuilder) -> RegexBuilder,
    {
        let builder = RegexBuilder::new(pattern);
        let configured = configure(builder);
        Self::condition(
            "path",
            Operator::Regex,
            Value::Regex(configured.into_regex_value()),
        )
    }

    /// Filter by repository
    pub fn repo(value: impl Into<String>) -> Self {
        Self::condition("repo", Operator::Equal, Value::String(value.into()))
    }

    // ========================================================================
    // Hierarchy Fields
    // ========================================================================

    /// Filter by parent symbol
    pub fn parent(value: impl Into<String>) -> Self {
        Self::condition("parent", Operator::Equal, Value::String(value.into()))
    }

    // ========================================================================
    // Content Fields
    // ========================================================================

    /// Filter by text content (regex only, default flags)
    pub fn text_matches(pattern: impl Into<String>) -> Self {
        let regex = RegexValue {
            pattern: pattern.into(),
            flags: RegexFlags::default(),
        };
        Self::condition("text", Operator::Regex, Value::Regex(regex))
    }

    /// Filter by text content (regex with custom flags via closure)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Multi-line text matching
    /// QueryBuilder::text_matches_with("^pub fn.*$", |rb| rb.multiline())
    /// ```
    pub fn text_matches_with<F>(pattern: impl Into<String>, configure: F) -> Self
    where
        F: FnOnce(RegexBuilder) -> RegexBuilder,
    {
        let builder = RegexBuilder::new(pattern);
        let configured = configure(builder);
        Self::condition(
            "text",
            Operator::Regex,
            Value::Regex(configured.into_regex_value()),
        )
    }

    // ========================================================================
    // Relation Predicates
    // ========================================================================

    /// Filter symbols that call the specified symbol
    pub fn callers(symbol: impl Into<String>) -> Self {
        Self::condition("callers", Operator::Equal, Value::String(symbol.into()))
    }

    /// Filter symbols called by the specified symbol
    pub fn callees(symbol: impl Into<String>) -> Self {
        Self::condition("callees", Operator::Equal, Value::String(symbol.into()))
    }

    /// Filter symbols that import the specified module
    pub fn imports(module: impl Into<String>) -> Self {
        Self::condition("imports", Operator::Equal, Value::String(module.into()))
    }

    /// Filter symbols that export something
    pub fn exports(value: impl Into<String>) -> Self {
        Self::condition("exports", Operator::Equal, Value::String(value.into()))
    }

    /// Filter symbols with the specified return type
    pub fn returns(type_name: impl Into<String>) -> Self {
        Self::condition("returns", Operator::Equal, Value::String(type_name.into()))
    }

    /// Filter symbols that reference the specified symbol
    pub fn references(symbol: impl Into<String>) -> Self {
        Self::condition("references", Operator::Equal, Value::String(symbol.into()))
    }

    // ========================================================================
    // Scope Predicates (P2-34)
    // ========================================================================

    /// Filter by scope (file, module, class, function, block)
    ///
    /// This targets the core `scope` field (enum type).
    pub fn scope(value: impl Into<String>) -> Self {
        Self::condition("scope", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by scope type (module, function, class, struct, method, block, etc.)
    ///
    /// This targets the `scope.type` compound field for nested scope filtering.
    pub fn scope_type(value: impl Into<String>) -> Self {
        Self::condition("scope.type", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by scope name
    pub fn scope_name(value: impl Into<String>) -> Self {
        Self::condition("scope.name", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by scope parent
    pub fn scope_parent(value: impl Into<String>) -> Self {
        Self::condition("scope.parent", Operator::Equal, Value::String(value.into()))
    }

    /// Filter by scope ancestor (transitive parent)
    pub fn scope_ancestor(value: impl Into<String>) -> Self {
        Self::condition(
            "scope.ancestor",
            Operator::Equal,
            Value::String(value.into()),
        )
    }

    // ========================================================================
    // Generic Field Access (for plugin fields)
    // ========================================================================

    /// Access any field by name with a value
    pub fn field(name: impl Into<String>, value: impl Into<Value>) -> Self {
        Self::condition_value(name.into(), Operator::Equal, value.into())
    }

    /// Access any field by name with regex match (default flags)
    pub fn field_matches(name: impl Into<String>, pattern: impl Into<String>) -> Self {
        let regex = RegexValue {
            pattern: pattern.into(),
            flags: RegexFlags::default(),
        };
        Self::condition_value(name.into(), Operator::Regex, Value::Regex(regex))
    }

    /// Access any field by name with regex match (custom flags via closure)
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Case-insensitive field matching
    /// QueryBuilder::field_matches_with("custom_field", "pattern.*", |rb| rb.case_insensitive())
    /// ```
    pub fn field_matches_with<F>(
        name: impl Into<String>,
        pattern: impl Into<String>,
        configure: F,
    ) -> Self
    where
        F: FnOnce(RegexBuilder) -> RegexBuilder,
    {
        let builder = RegexBuilder::new(pattern);
        let configured = configure(builder);
        Self::condition_value(
            name.into(),
            Operator::Regex,
            Value::Regex(configured.into_regex_value()),
        )
    }

    /// Numeric comparison: field > value
    pub fn field_gt(name: impl Into<String>, value: i64) -> Self {
        Self::condition_value(name.into(), Operator::Greater, Value::Number(value))
    }

    /// Numeric comparison: field >= value
    pub fn field_gte(name: impl Into<String>, value: i64) -> Self {
        Self::condition_value(name.into(), Operator::GreaterEq, Value::Number(value))
    }

    /// Numeric comparison: field < value
    pub fn field_lt(name: impl Into<String>, value: i64) -> Self {
        Self::condition_value(name.into(), Operator::Less, Value::Number(value))
    }

    /// Numeric comparison: field <= value
    pub fn field_lte(name: impl Into<String>, value: i64) -> Self {
        Self::condition_value(name.into(), Operator::LessEq, Value::Number(value))
    }

    // ========================================================================
    // Private Helpers
    // ========================================================================

    /// Create a condition with a static field name (used by core field methods)
    fn condition(field: &'static str, operator: Operator, value: Value) -> Self {
        Self {
            expr: BuilderExpr::Condition(ConditionBuilder::new_static(field, operator, value)),
            errors: Vec::new(),
        }
    }

    /// Create a condition with a dynamic field name (used by generic field methods)
    fn condition_value(field: String, operator: Operator, value: Value) -> Self {
        Self {
            expr: BuilderExpr::Condition(ConditionBuilder::new(field, operator, value)),
            errors: Vec::new(),
        }
    }
}

// ============================================================================
// Boolean Combinators
// ============================================================================

impl QueryBuilder {
    /// Static constructor: AND of multiple conditions
    pub fn all(conditions: Vec<QueryBuilder>) -> Self {
        let errors = conditions.iter().flat_map(|c| c.errors.clone()).collect();
        Self {
            expr: BuilderExpr::And(conditions),
            errors,
        }
    }

    /// Static constructor: OR of multiple conditions
    pub fn any(conditions: Vec<QueryBuilder>) -> Self {
        let errors = conditions.iter().flat_map(|c| c.errors.clone()).collect();
        Self {
            expr: BuilderExpr::Or(conditions),
            errors,
        }
    }

    /// Chainable: combine with AND
    pub fn and(self, other: QueryBuilder) -> Self {
        // Merge errors from both operands
        let mut errors = self.errors;
        errors.extend(other.errors.clone());

        match self.expr {
            BuilderExpr::Empty => Self {
                expr: other.expr,
                errors,
            },
            BuilderExpr::And(mut exprs) => {
                exprs.push(other);
                Self {
                    expr: BuilderExpr::And(exprs),
                    errors,
                }
            }
            _ => Self {
                expr: BuilderExpr::And(vec![
                    Self {
                        expr: self.expr,
                        errors: Vec::new(),
                    },
                    other,
                ]),
                errors,
            },
        }
    }

    /// Chainable: combine with OR
    pub fn or(self, other: QueryBuilder) -> Self {
        // Merge errors from both operands
        let mut errors = self.errors;
        errors.extend(other.errors.clone());

        match self.expr {
            BuilderExpr::Empty => Self {
                expr: other.expr,
                errors,
            },
            BuilderExpr::Or(mut exprs) => {
                exprs.push(other);
                Self {
                    expr: BuilderExpr::Or(exprs),
                    errors,
                }
            }
            _ => Self {
                expr: BuilderExpr::Or(vec![
                    Self {
                        expr: self.expr,
                        errors: Vec::new(),
                    },
                    other,
                ]),
                errors,
            },
        }
    }

    /// Chainable: combine with AND NOT
    pub fn and_not(self, other: QueryBuilder) -> Self {
        self.and(Self::negate(other))
    }

    /// Static constructor: negate expression
    ///
    /// Named `negate` to avoid confusion with `std::ops::Not::not`.
    /// Use this to create `NOT <expr>` conditions.
    pub fn negate(builder: QueryBuilder) -> Self {
        let errors = builder.errors.clone();
        Self {
            expr: BuilderExpr::Not(Box::new(builder)),
            errors,
        }
    }
}

// ============================================================================
// Build Methods
// ============================================================================

impl QueryBuilder {
    /// Build the query with default field registry validation
    ///
    /// # Errors
    ///
    /// Returns `BuildError` if:
    /// - Unknown field names are used
    /// - Operators are incompatible with field types
    /// - Value types don't match field types
    /// - Enum values are invalid
    /// - Regex patterns are syntactically invalid
    /// - The query is empty (no conditions)
    pub fn build(self) -> Result<Arc<QueryAST>, BuildError> {
        let registry = FieldRegistry::with_core_fields();
        self.build_with_registry(&registry)
    }

    /// Build with custom field registry (for plugin fields)
    ///
    /// This allows validation against a registry that includes plugin-specific
    /// fields in addition to core fields.
    ///
    /// # Errors
    ///
    /// Same as `build()`.
    pub fn build_with_registry(
        self,
        registry: &FieldRegistry,
    ) -> Result<Arc<QueryAST>, BuildError> {
        // Report any accumulated errors
        if !self.errors.is_empty() {
            return Err(BuildError::Multiple(self.errors));
        }

        // Convert builder expression to AST
        let expr = self.into_expr(registry)?;

        Ok(Arc::new(QueryAST {
            root: expr,
            span: Span::synthetic(),
        }))
    }

    fn into_expr(self, registry: &FieldRegistry) -> Result<Expr, BuildError> {
        match self.expr {
            BuilderExpr::Empty => Err(BuildError::EmptyQuery),
            BuilderExpr::Condition(ref cond) => {
                // Validate field, operator, value, and enum constraints
                Self::validate_condition(cond, registry)?;
                // Clone the condition to allow consumption by into_condition
                Ok(Expr::Condition(cond.clone().into_condition(registry)))
            }
            BuilderExpr::And(exprs) => {
                let children: Result<Vec<_>, _> =
                    exprs.into_iter().map(|e| e.into_expr(registry)).collect();
                Ok(Expr::And(children?))
            }
            BuilderExpr::Or(exprs) => {
                let children: Result<Vec<_>, _> =
                    exprs.into_iter().map(|e| e.into_expr(registry)).collect();
                Ok(Expr::Or(children?))
            }
            BuilderExpr::Not(inner) => Ok(Expr::Not(Box::new(inner.into_expr(registry)?))),
        }
    }

    fn validate_condition(
        cond: &ConditionBuilder,
        registry: &FieldRegistry,
    ) -> Result<(), BuildError> {
        // Get field descriptor (resolves aliases)
        let descriptor = registry
            .get(cond.field())
            .ok_or_else(|| BuildError::UnknownField {
                field: cond.field().to_string(),
                available: registry.field_names().join(", "),
            })?;

        // Check operator is valid for field type
        if !descriptor.supports_operator(cond.operator()) {
            return Err(BuildError::InvalidOperator {
                field: cond.field().to_string(),
                operator: cond.operator().clone(),
                field_type: format!("{:?}", descriptor.field_type),
            });
        }

        // Check value type matches field type
        Self::validate_value_type(cond.field(), &descriptor.field_type, cond.value())?;

        // Validate regex patterns early (FR-5)
        // This catches invalid patterns from convenience methods like name_matches()
        Self::validate_regex_pattern(cond.value())?;

        // Check enum constraints for applicable fields
        Self::validate_enum_value(cond.field(), cond.value(), &descriptor.field_type)?;

        Ok(())
    }

    fn validate_regex_pattern(value: &Value) -> Result<(), BuildError> {
        if let Value::Regex(regex_value) = value {
            // Check if pattern contains lookaround assertions (FT-C.1: Support lookaround)
            // Aligned with validator.rs behavior to accept the same patterns.
            let has_lookaround = regex_value.pattern.contains("(?=")
                || regex_value.pattern.contains("(?!")
                || regex_value.pattern.contains("(?<=")
                || regex_value.pattern.contains("(?<!");

            if has_lookaround {
                // Use fancy-regex for lookaround support
                fancy_regex::Regex::new(&regex_value.pattern).map_err(|e| {
                    BuildError::InvalidFancyRegex {
                        pattern: regex_value.pattern.clone(),
                        error: e.to_string(),
                    }
                })?;
            } else {
                // Use standard regex for performance (validate with flags applied)
                let mut builder = regex::RegexBuilder::new(&regex_value.pattern);
                builder.case_insensitive(regex_value.flags.case_insensitive);
                builder.multi_line(regex_value.flags.multiline);
                builder.dot_matches_new_line(regex_value.flags.dot_all);
                builder.build()?;
            }
        }
        Ok(())
    }

    fn validate_enum_value(
        field: &str,
        value: &Value,
        field_type: &FieldType,
    ) -> Result<(), BuildError> {
        // Extract enum values from the field type
        // This ensures validation stays in sync with FieldRegistry::core_fields()
        if let (FieldType::Enum(valid), Value::String(s)) = (field_type, value)
            && !valid.contains(&s.as_str())
        {
            return Err(BuildError::InvalidEnumValue {
                field: field.to_string(),
                value: s.clone(),
                valid: valid.join(", "),
            });
        }

        Ok(())
    }

    fn validate_value_type(
        field: &str,
        field_type: &FieldType,
        value: &Value,
    ) -> Result<(), BuildError> {
        // Match value types to field types, aligned with validator.rs behavior.
        // Regex values are valid for String, Path, and Enum fields (e.g., kind~=/function|method/).
        let is_valid = matches!(
            (field_type, value),
            (
                FieldType::String | FieldType::Path | FieldType::Enum(_),
                Value::String(_) | Value::Regex(_)
            ) | (FieldType::Number, Value::Number(_))
                | (FieldType::Bool, Value::Boolean(_)) // enum regex: kind~=/function|method/
        );

        if !is_valid {
            return Err(BuildError::ValueTypeMismatch {
                field: field.to_string(),
                expected: format!("{field_type:?}"),
                actual: value.type_name().to_string(),
            });
        }

        Ok(())
    }
}

impl Default for QueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Conversion from Value types for generic field() method
// ============================================================================

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Number(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Boolean(b)
    }
}
