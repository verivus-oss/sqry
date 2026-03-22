//! Semantic validation for query ASTs
//!
//! This module provides validation for parsed query ASTs, checking:
//! - Field names against the field registry
//! - Operator compatibility with field types
//! - Value type matching
//! - Regex pattern validity
//! - Contradiction detection
//!
//! The validator provides helpful error messages with suggestions for typos
//! and clear explanations of validation failures.
//!
//! # Regex Support (FT-C.1)
//!
//! The validator supports both standard regex patterns (via the `regex` crate)
//! and advanced patterns with lookaround assertions (via the `fancy-regex` crate).
//!
//! Lookaround support includes:
//! - Positive lookahead: `(?=...)`
//! - Negative lookahead: `(?!...)`
//! - Positive lookbehind: `(?<=...)`
//! - Negative lookbehind: `(?<!...)`
//!
//! The validator automatically detects lookaround patterns and uses the
//! appropriate regex engine.

use regex::Regex;
use std::collections::HashMap;

use super::error::ValidationError;
use super::registry::FieldRegistry;
use super::types::{Condition, Expr, Field, FieldDescriptor, FieldType, Operator, Span, Value};

const SAFE_FUZZY_FIELDS: &[&str] = &[
    "kind",
    "path",
    "lang",
    "repo",
    "parent",
    "scope.type",
    "scope.name",
    "scope.parent",
    "scope.ancestor",
    "callers",
    "callees",
    "imports",
    "exports",
    "returns",
    "references",
];

/// Semantic validator for query ASTs
///
/// Validates queries against a field registry to ensure:
/// - All field names exist
/// - Operators are compatible with field types
/// - Values match expected types
/// - Regex patterns are valid
///
/// # Example
///
/// ```
/// use sqry_core::query::registry::FieldRegistry;
/// use sqry_core::query::validator::Validator;
/// use sqry_core::query::types::{Expr, Condition, Field, Operator, Value, Span};
///
/// let registry = FieldRegistry::with_core_fields();
/// let validator = Validator::new(registry);
///
/// let condition = Expr::Condition(Condition {
///     field: Field::new("kind"),
///     operator: Operator::Equal,
///     value: Value::String("function".to_string()),
///     span: Span::default(),
/// });
///
/// assert!(validator.validate(&condition).is_ok());
/// ```
/// Configuration for validation behaviour.
#[derive(Clone, Copy, Debug)]
pub struct ValidationOptions {
    /// Enable fuzzy field correction (opt-in).
    pub fuzzy_fields: bool,
    /// Maximum edit distance allowed for fuzzy field correction.
    pub fuzzy_field_distance: usize,
}

impl Default for ValidationOptions {
    fn default() -> Self {
        Self {
            fuzzy_fields: false,
            fuzzy_field_distance: 2,
        }
    }
}

/// Validator for query expressions and field/value semantics.
pub struct Validator {
    registry: FieldRegistry,
    options: ValidationOptions,
}

impl Validator {
    /// Create a new validator with the given field registry
    #[must_use]
    pub fn new(registry: FieldRegistry) -> Self {
        Self {
            registry,
            options: ValidationOptions::default(),
        }
    }

    /// Create a new validator with options
    #[must_use]
    pub fn with_options(registry: FieldRegistry, options: ValidationOptions) -> Self {
        Self { registry, options }
    }

    /// Validate a query expression
    ///
    /// Returns `Ok(())` if the expression is valid, or a `ValidationError` if validation fails.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] when the expression uses unknown fields, invalid operators, or mismatched value types.
    pub fn validate(&self, expr: &Expr) -> Result<(), ValidationError> {
        self.validate_node_with_depth(expr, 0)
    }

    /// Normalize field names using fuzzy options (when enabled) and return a new expression tree.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError`] when normalization fails or fuzzy correction is unsafe.
    pub fn normalize_expr(&self, expr: &Expr) -> Result<Expr, ValidationError> {
        match expr {
            Expr::And(operands) => Ok(Expr::And(self.normalize_operands(operands)?)),
            Expr::Or(operands) => Ok(Expr::Or(self.normalize_operands(operands)?)),
            Expr::Not(op) => Ok(Expr::Not(Box::new(self.normalize_expr(op)?))),
            Expr::Condition(cond) => Ok(Expr::Condition(self.normalize_condition(cond)?)),
            Expr::Join(join) => Ok(Expr::Join(crate::query::types::JoinExpr {
                left: Box::new(self.normalize_expr(&join.left)?),
                edge: join.edge.clone(),
                right: Box::new(self.normalize_expr(&join.right)?),
                span: join.span.clone(),
            })),
        }
    }

    /// Validate a single AST node recursively, tracking subquery nesting depth.
    fn validate_node_with_depth(
        &self,
        node: &Expr,
        subquery_depth: usize,
    ) -> Result<(), ValidationError> {
        match node {
            Expr::And(operands) | Expr::Or(operands) => {
                for operand in operands {
                    self.validate_node_with_depth(operand, subquery_depth)?;
                }
                Ok(())
            }
            Expr::Not(operand) => self.validate_node_with_depth(operand, subquery_depth),
            Expr::Condition(condition) => {
                self.validate_condition(condition)?;
                // If the value is a subquery, validate its inner expression
                // with incremented depth
                if let Value::Subquery(inner) = &condition.value {
                    let new_depth = subquery_depth + 1;
                    if new_depth > crate::query::types::MAX_SUBQUERY_DEPTH {
                        return Err(ValidationError::SubqueryDepthExceeded {
                            depth: new_depth,
                            max_depth: crate::query::types::MAX_SUBQUERY_DEPTH,
                            span: condition.span.clone(),
                        });
                    }
                    self.validate_node_with_depth(inner, new_depth)?;
                }
                Ok(())
            }
            Expr::Join(join) => {
                self.validate_node_with_depth(&join.left, subquery_depth)?;
                self.validate_node_with_depth(&join.right, subquery_depth)?;
                Ok(())
            }
        }
    }

    /// Validate a condition
    fn validate_condition(&self, condition: &Condition) -> Result<(), ValidationError> {
        let field_name = condition.field.as_str();
        let field_desc = self.resolve_field_descriptor(condition)?;

        Self::validate_operator(field_name, field_desc, condition)?;
        Self::validate_value_type(field_name, field_desc, condition)?;
        Self::validate_enum_value(field_name, field_desc, condition)?;
        Self::validate_regex_pattern(condition)?;

        Ok(())
    }

    fn resolve_field_descriptor<'a>(
        &'a self,
        condition: &Condition,
    ) -> Result<&'a FieldDescriptor, ValidationError> {
        let field_name = condition.field.as_str();
        self.registry.get(field_name).ok_or_else(|| {
            let suggestion = self.suggest_field(field_name);
            ValidationError::UnknownField {
                field: field_name.to_string(),
                suggestion,
                span: condition.span.clone(),
            }
        })
    }

    fn validate_operator(
        field_name: &str,
        field_desc: &FieldDescriptor,
        condition: &Condition,
    ) -> Result<(), ValidationError> {
        if field_desc.supports_operator(&condition.operator) {
            return Ok(());
        }

        Err(ValidationError::InvalidOperator {
            field: field_name.to_string(),
            operator: condition.operator.clone(),
            valid_operators: field_desc.operators.to_vec(),
            span: condition.span.clone(),
        })
    }

    fn validate_value_type(
        field_name: &str,
        field_desc: &FieldDescriptor,
        condition: &Condition,
    ) -> Result<(), ValidationError> {
        let is_value_type_valid = match (&condition.operator, &condition.value) {
            // Regex values are valid with ~= operator for String/Enum/Path fields
            (Operator::Regex, Value::Regex(_)) => matches!(
                field_desc.field_type,
                FieldType::String | FieldType::Enum(_) | FieldType::Path
            ),
            // For all other cases, use standard type matching
            _ => field_desc.matches_value_type(&condition.value),
        };

        if is_value_type_valid {
            return Ok(());
        }

        Err(ValidationError::TypeMismatch {
            field: field_name.to_string(),
            expected: field_desc.field_type.clone(),
            got: condition.value.clone(),
            span: condition.span.clone(),
        })
    }

    fn validate_enum_value(
        field_name: &str,
        field_desc: &FieldDescriptor,
        condition: &Condition,
    ) -> Result<(), ValidationError> {
        if let FieldType::Enum(allowed_values) = &field_desc.field_type
            && let Value::String(value) = &condition.value
            && !allowed_values.contains(&value.as_str())
        {
            return Err(ValidationError::InvalidEnumValue {
                field: field_name.to_string(),
                value: value.clone(),
                valid_values: allowed_values.clone(),
                span: condition.span.clone(),
            });
        }

        Ok(())
    }

    fn validate_regex_pattern(condition: &Condition) -> Result<(), ValidationError> {
        let Value::Regex(regex_val) = &condition.value else {
            return Ok(());
        };

        // Check if pattern contains lookaround assertions
        let has_lookaround = regex_val.pattern.contains("(?=")
            || regex_val.pattern.contains("(?!")
            || regex_val.pattern.contains("(?<=")
            || regex_val.pattern.contains("(?<!");

        if has_lookaround {
            // Use fancy-regex for lookaround support
            if let Err(e) = fancy_regex::Regex::new(&regex_val.pattern) {
                return Err(ValidationError::InvalidRegexPattern {
                    pattern: regex_val.pattern.clone(),
                    error: e.to_string(),
                    span: condition.span.clone(),
                });
            }
        } else {
            // Use standard regex for performance
            if let Err(e) = Regex::new(&regex_val.pattern) {
                return Err(ValidationError::InvalidRegexPattern {
                    pattern: regex_val.pattern.clone(),
                    error: e.to_string(),
                    span: condition.span.clone(),
                });
            }
        }

        Ok(())
    }

    fn normalize_operands(&self, operands: &[Expr]) -> Result<Vec<Expr>, ValidationError> {
        let mut normalized = Vec::with_capacity(operands.len());
        for operand in operands {
            normalized.push(self.normalize_expr(operand)?);
        }
        Ok(normalized)
    }

    /// Detect contradictions in the query
    ///
    /// Returns warnings for impossible queries, such as:
    /// - `kind:function AND kind:class` (same field with different values)
    /// - `async:true AND async:false` (boolean contradiction)
    #[allow(clippy::only_used_in_recursion)]
    #[must_use]
    pub fn detect_contradictions(&self, expr: &Expr) -> Vec<ContradictionWarning> {
        let mut warnings = Vec::new();

        if let Expr::And(operands) = expr {
            warnings.extend(Self::detect_exact_match_contradictions(operands));
        }

        warnings.extend(self.detect_nested_contradictions(expr));

        warnings
    }

    fn detect_exact_match_contradictions(operands: &[Expr]) -> Vec<ContradictionWarning> {
        let constraints = Self::collect_exact_constraints(operands);
        constraints
            .into_iter()
            .filter_map(|(field, values)| {
                Self::contradiction_for_field(operands, field.as_str(), &values)
            })
            .collect()
    }

    fn detect_nested_contradictions(&self, expr: &Expr) -> Vec<ContradictionWarning> {
        match expr {
            Expr::And(operands) | Expr::Or(operands) => operands
                .iter()
                .flat_map(|operand| self.detect_contradictions(operand))
                .collect(),
            Expr::Not(operand) => self.detect_contradictions(operand),
            Expr::Condition(_) => Vec::new(),
            Expr::Join(join) => {
                let mut warnings = self.detect_contradictions(&join.left);
                warnings.extend(self.detect_contradictions(&join.right));
                warnings
            }
        }
    }

    fn collect_exact_constraints(operands: &[Expr]) -> HashMap<String, Vec<(String, usize)>> {
        let mut constraints: HashMap<String, Vec<(String, usize)>> = HashMap::new();

        for (idx, operand) in operands.iter().enumerate() {
            if let Expr::Condition(condition) = operand
                && condition.operator == Operator::Equal
            {
                if let Some(value) = condition.value.as_string() {
                    constraints
                        .entry(condition.field.as_str().to_string())
                        .or_default()
                        .push((value.to_string(), idx));
                } else if let Value::Boolean(value) = &condition.value {
                    constraints
                        .entry(condition.field.as_str().to_string())
                        .or_default()
                        .push((value.to_string(), idx));
                }
            }
        }

        constraints
    }

    fn contradiction_for_field(
        operands: &[Expr],
        field: &str,
        values: &[(String, usize)],
    ) -> Option<ContradictionWarning> {
        if values.len() <= 1 {
            return None;
        }

        let unique_values: Vec<_> = values
            .iter()
            .map(|(v, _)| v.as_str())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        if unique_values.len() <= 1 {
            return None;
        }

        let merged_span = Self::merge_operand_spans(operands, values);
        let value_list = unique_values.join("' and '");
        Some(ContradictionWarning {
            message: format!("Query is impossible: field '{field}' cannot be both '{value_list}'"),
            span: merged_span,
        })
    }

    fn merge_operand_spans(operands: &[Expr], values: &[(String, usize)]) -> Span {
        values
            .iter()
            .filter_map(|(_, idx)| match &operands[*idx] {
                Expr::Condition(cond) => Some(cond.span.clone()),
                _ => None,
            })
            .fold(None, |acc: Option<Span>, span| {
                Some(acc.map_or(span.clone(), |s| s.merge(&span)))
            })
            .unwrap_or_default()
    }

    /// Suggest a field name for a typo using Levenshtein distance
    ///
    /// Returns the closest matching field name if the edit distance is ≤ 2.
    /// Matching is case-insensitive to handle case typos like "KIND" → "kind".
    fn suggest_field(&self, input: &str) -> Option<String> {
        self.suggest_field_with_threshold(input, 2)
            .into_iter()
            .next()
    }

    fn suggest_field_with_threshold(&self, input: &str, max_distance: usize) -> Vec<String> {
        let input_lower = input.to_lowercase();
        let mut best_match: Option<usize> = None;
        let mut candidates: Vec<String> = Vec::new();

        for field_name in self.registry.field_names() {
            // Check for exact case-insensitive match first
            if field_name.to_lowercase() == input_lower {
                return vec![field_name.to_string()];
            }

            // Otherwise use Levenshtein distance
            let distance = levenshtein_distance(&input_lower, &field_name.to_lowercase());

            // Only suggest if distance within threshold
            if distance <= max_distance {
                match best_match {
                    Some(best_dist) if distance < best_dist => {
                        best_match = Some(distance);
                        candidates.clear();
                        candidates.push(field_name.to_string());
                    }
                    Some(best_dist) if distance == best_dist => {
                        candidates.push(field_name.to_string());
                    }
                    None => {
                        best_match = Some(distance);
                        candidates.push(field_name.to_string());
                    }
                    _ => {}
                }
            }
        }

        candidates
    }

    fn normalize_condition(&self, condition: &Condition) -> Result<Condition, ValidationError> {
        // Fast path: exact match
        if self.registry.get(condition.field.as_str()).is_some() {
            return Ok(condition.clone());
        }

        // Fuzzy disabled: reject unknown fields outright.
        if !self.options.fuzzy_fields {
            return Err(ValidationError::UnknownField {
                field: condition.field.as_str().to_string(),
                suggestion: self.suggest_field(condition.field.as_str()),
                span: condition.span.clone(),
            });
        }

        // Try fuzzy suggestion within threshold
        let suggestions = self.suggest_field_with_threshold(
            condition.field.as_str(),
            self.options.fuzzy_field_distance,
        );
        match suggestions.len() {
            1 => {
                let mut corrected = condition.clone();
                let candidate = suggestions[0].clone();

                // Do not auto-correct fields that are prone to ambiguity (e.g., "name").
                // Users must spell these exactly or accept explicit errors.
                if !SAFE_FUZZY_FIELDS.contains(&candidate.as_str()) {
                    return Err(ValidationError::UnsafeFuzzyCorrection {
                        input: condition.field.as_str().to_string(),
                        suggestion: candidate,
                        span: condition.span.clone(),
                    });
                }

                corrected.field = Field::new(candidate);
                Ok(corrected)
            }
            n if n > 1 => Err(ValidationError::UnknownField {
                field: condition.field.as_str().to_string(),
                suggestion: Some(format!("ambiguous: {}", suggestions.join(", "))),
                span: condition.span.clone(),
            }),
            _ => Err(ValidationError::UnknownField {
                field: condition.field.as_str().to_string(),
                suggestion: None,
                span: condition.span.clone(),
            }),
        }
    }
}

/// Warning about a potential contradiction in the query
#[derive(Debug, Clone, PartialEq)]
pub struct ContradictionWarning {
    /// Warning message
    pub message: String,
    /// Location of the contradiction
    pub span: Span,
}

/// Compute Levenshtein distance (edit distance) between two strings
///
/// Returns the minimum number of single-character edits (insertions, deletions, substitutions)
/// required to change one string into the other.
#[allow(clippy::needless_range_loop)]
fn levenshtein_distance(s1: &str, s2: &str) -> usize {
    let len1 = s1.chars().count();
    let len2 = s2.chars().count();

    // Create matrix
    let mut matrix = vec![vec![0; len2 + 1]; len1 + 1];

    // Initialize first row and column
    for i in 0..=len1 {
        matrix[i][0] = i;
    }
    for j in 0..=len2 {
        matrix[0][j] = j;
    }

    // Fill matrix
    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    for (i, c1) in s1_chars.iter().enumerate() {
        for (j, c2) in s2_chars.iter().enumerate() {
            let cost = usize::from(c1 != c2);

            matrix[i + 1][j + 1] = std::cmp::min(
                std::cmp::min(
                    matrix[i][j + 1] + 1, // deletion
                    matrix[i + 1][j] + 1, // insertion
                ),
                matrix[i][j] + cost, // substitution
            );
        }
    }

    matrix[len1][len2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::types::{Field, Span};

    #[test]
    fn test_levenshtein_distance() {
        assert_eq!(levenshtein_distance("", ""), 0);
        assert_eq!(levenshtein_distance("hello", "hello"), 0);
        assert_eq!(levenshtein_distance("hello", "hallo"), 1);
        assert_eq!(levenshtein_distance("kind", "knd"), 1);
        assert_eq!(levenshtein_distance("kind", "kond"), 1);
        assert_eq!(levenshtein_distance("kind", "king"), 1);
        assert_eq!(levenshtein_distance("kind", "xyz"), 4);
    }

    #[test]
    fn test_validate_valid_condition() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::String("function".to_string()),
            span: Span::default(),
        });

        assert!(validator.validate(&condition).is_ok());
    }

    #[test]
    fn test_validate_unknown_field() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("unknown"),
            operator: Operator::Equal,
            value: Value::String("value".to_string()),
            span: Span::default(),
        });

        let result = validator.validate(&condition);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::UnknownField { .. }
        ));
    }

    #[test]
    fn test_suggest_field_typo() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let suggestion = validator.suggest_field("knd");
        assert_eq!(suggestion, Some("kind".to_string()));

        let suggestion = validator.suggest_field("kond");
        assert_eq!(suggestion, Some("kind".to_string()));

        let suggestion = validator.suggest_field("nme");
        assert_eq!(suggestion, Some("name".to_string()));
    }

    #[test]
    fn test_suggest_field_no_match() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let suggestion = validator.suggest_field("xyz");
        assert!(suggestion.is_none());

        let suggestion = validator.suggest_field("foobar");
        assert!(suggestion.is_none());
    }

    #[test]
    fn test_fuzzy_field_correction_enabled() {
        let registry = FieldRegistry::with_core_fields();
        let options = ValidationOptions {
            fuzzy_fields: true,
            fuzzy_field_distance: 2,
        };
        let validator = Validator::with_options(registry, options);
        let cond = Condition {
            field: Field::new("knd"),
            operator: Operator::Equal,
            value: Value::String("function".to_string()),
            span: Span::default(),
        };
        let normalized = validator
            .normalize_condition(&cond)
            .expect("should normalize");
        assert_eq!(normalized.field.as_str(), "kind");
    }

    #[test]
    fn test_fuzzy_field_ambiguous_rejected() {
        let registry = FieldRegistry::with_core_fields();
        let options = ValidationOptions {
            fuzzy_fields: true,
            fuzzy_field_distance: 2,
        };
        let validator = Validator::with_options(registry, options);
        let cond = Condition {
            field: Field::new("nam"),
            operator: Operator::Equal,
            value: Value::String("foo".to_string()),
            span: Span::default(),
        };
        let result = validator.normalize_condition(&cond);
        assert!(result.is_err(), "ambiguous correction must error");
    }

    #[test]
    fn test_fuzzy_field_disabled_rejects() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);
        let cond = Condition {
            field: Field::new("knd"),
            operator: Operator::Equal,
            value: Value::String("function".to_string()),
            span: Span::default(),
        };
        let result = validator.normalize_condition(&cond);
        assert!(result.is_err(), "disabled fuzzy should reject typos");
    }

    #[test]
    fn test_fuzzy_field_non_whitelisted_returns_unsafe_error() {
        // Add a custom field that is NOT in SAFE_FUZZY_FIELDS whitelist
        let mut registry = FieldRegistry::with_core_fields();
        registry.add_field(super::super::types::FieldDescriptor {
            name: "custom",
            field_type: FieldType::String,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "A custom field for testing",
        });
        let options = ValidationOptions {
            fuzzy_fields: true,
            fuzzy_field_distance: 2,
        };
        let validator = Validator::with_options(registry, options);
        // "custm" is a typo for "custom" (distance 1)
        let cond = Condition {
            field: Field::new("custm"),
            operator: Operator::Equal,
            value: Value::String("test".to_string()),
            span: Span::default(),
        };
        let result = validator.normalize_condition(&cond);
        assert!(result.is_err(), "non-whitelisted field should error");
        assert!(
            matches!(
                result.unwrap_err(),
                ValidationError::UnsafeFuzzyCorrection { .. }
            ),
            "should return UnsafeFuzzyCorrection, not UnknownField"
        );
    }

    #[test]
    fn test_suggest_field_case_insensitive() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        // Exact case-insensitive match
        let suggestion = validator.suggest_field("KIND");
        assert_eq!(suggestion, Some("kind".to_string()));

        let suggestion = validator.suggest_field("Name");
        assert_eq!(suggestion, Some("name".to_string()));

        // Case-insensitive with typo
        let suggestion = validator.suggest_field("KND");
        assert_eq!(suggestion, Some("kind".to_string()));
    }

    #[test]
    fn test_validate_invalid_operator() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Greater,
            value: Value::String("function".to_string()),
            span: Span::default(),
        });

        let result = validator.validate(&condition);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidOperator { .. }
        ));
    }

    #[test]
    fn test_validate_type_mismatch() {
        let registry = FieldRegistry::with_core_fields();
        let _validator = Validator::new(registry);

        // Add an async field for testing
        let mut registry = FieldRegistry::with_core_fields();
        registry.add_field(super::super::types::FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        });
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("async"),
            operator: Operator::Equal,
            value: Value::Number(123),
            span: Span::default(),
        });

        let result = validator.validate(&condition);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::TypeMismatch { .. }
        ));
    }

    #[test]
    fn test_validate_invalid_enum_value() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::String("invalid_kind".to_string()),
            span: Span::default(),
        });

        let result = validator.validate(&condition);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidEnumValue { .. }
        ));
    }

    #[test]
    fn test_validate_valid_enum_value() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let valid_kinds = ["function", "method", "class", "struct", "trait"];

        for kind in &valid_kinds {
            let condition = Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String((*kind).to_string()),
                span: Span::default(),
            });

            assert!(validator.validate(&condition).is_ok());
        }
    }

    #[test]
    fn test_validate_invalid_regex() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("name"),
            operator: Operator::Regex,
            value: Value::Regex(super::super::types::RegexValue {
                pattern: "[invalid".to_string(),
                flags: super::super::types::RegexFlags::default(),
            }),
            span: Span::default(),
        });

        let result = validator.validate(&condition);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ValidationError::InvalidRegexPattern { .. }
        ));
    }

    #[test]
    fn test_validate_valid_regex() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let condition = Expr::Condition(Condition {
            field: Field::new("name"),
            operator: Operator::Regex,
            value: Value::Regex(super::super::types::RegexValue {
                pattern: "^test_.*".to_string(),
                flags: super::super::types::RegexFlags::default(),
            }),
            span: Span::default(),
        });

        assert!(validator.validate(&condition).is_ok());
    }

    #[test]
    fn test_detect_contradiction_enum() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("class".to_string()),
                span: Span::default(),
            }),
        ]);

        let warnings = validator.detect_contradictions(&expr);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("kind"));
        assert!(warnings[0].message.contains("function"));
        assert!(warnings[0].message.contains("class"));
    }

    #[test]
    fn test_detect_contradiction_boolean() {
        let mut registry = FieldRegistry::with_core_fields();
        registry.add_field(super::super::types::FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        });
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("async"),
                operator: Operator::Equal,
                value: Value::Boolean(true),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("async"),
                operator: Operator::Equal,
                value: Value::Boolean(false),
                span: Span::default(),
            }),
        ]);

        let warnings = validator.detect_contradictions(&expr);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("async"));
    }

    #[test]
    fn test_no_contradiction_or() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let expr = Expr::Or(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("class".to_string()),
                span: Span::default(),
            }),
        ]);

        let warnings = validator.detect_contradictions(&expr);
        assert_eq!(warnings.len(), 0);
    }

    #[test]
    fn test_no_contradiction_different_fields() {
        let mut registry = FieldRegistry::with_core_fields();
        registry.add_field(super::super::types::FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        });
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("async"),
                operator: Operator::Equal,
                value: Value::Boolean(true),
                span: Span::default(),
            }),
        ]);

        let warnings = validator.detect_contradictions(&expr);
        assert_eq!(warnings.len(), 0);
    }

    #[test]
    fn test_validate_and_expression() {
        let mut registry = FieldRegistry::with_core_fields();
        registry.add_field(super::super::types::FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        });
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("async"),
                operator: Operator::Equal,
                value: Value::Boolean(true),
                span: Span::default(),
            }),
        ]);

        assert!(validator.validate(&expr).is_ok());
    }

    #[test]
    fn test_validate_or_expression() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let expr = Expr::Or(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("class".to_string()),
                span: Span::default(),
            }),
        ]);

        assert!(validator.validate(&expr).is_ok());
    }

    #[test]
    fn test_validate_not_expression() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let expr = Expr::Not(Box::new(Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::String("function".to_string()),
            span: Span::default(),
        })));

        assert!(validator.validate(&expr).is_ok());
    }

    #[test]
    fn test_validate_nested_expression() {
        let mut registry = FieldRegistry::with_core_fields();
        registry.add_field(super::super::types::FieldDescriptor {
            name: "async",
            field_type: FieldType::Bool,
            operators: &[Operator::Equal],
            indexed: false,
            doc: "Whether function is async",
        });
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Or(vec![
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::default(),
                }),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("method".to_string()),
                    span: Span::default(),
                }),
            ]),
            Expr::Condition(Condition {
                field: Field::new("async"),
                operator: Operator::Equal,
                value: Value::Boolean(true),
                span: Span::default(),
            }),
        ]);

        assert!(validator.validate(&expr).is_ok());
    }

    #[test]
    fn test_detect_nested_contradiction() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        // Nested: (kind:function AND kind:class) OR async:true
        // Should detect contradiction in left branch
        let expr = Expr::Or(vec![
            Expr::And(vec![
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("function".to_string()),
                    span: Span::default(),
                }),
                Expr::Condition(Condition {
                    field: Field::new("kind"),
                    operator: Operator::Equal,
                    value: Value::String("class".to_string()),
                    span: Span::default(),
                }),
            ]),
            Expr::Condition(Condition {
                field: Field::new("name"),
                operator: Operator::Equal,
                value: Value::String("test".to_string()),
                span: Span::default(),
            }),
        ]);

        let warnings = validator.detect_contradictions(&expr);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].message.contains("kind"));
        assert!(warnings[0].message.contains("function"));
        assert!(warnings[0].message.contains("class"));
    }

    #[test]
    fn test_contradiction_warning_has_span() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("function".to_string()),
                span: Span::with_position(0, 13, 1, 1),
            }),
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("class".to_string()),
                span: Span::with_position(18, 28, 1, 19),
            }),
        ]);

        let warnings = validator.detect_contradictions(&expr);
        assert_eq!(warnings.len(), 1);

        // Verify span is present and covers both conditions
        assert_eq!(warnings[0].span.start, 0);
        assert_eq!(warnings[0].span.end, 28);
    }

    // ================================================================
    // Subquery depth validation tests
    // ================================================================

    /// Build a subquery chain nested to the given depth.
    ///
    /// Depth 1: `callers:(kind:function)`
    /// Depth 2: `callers:(callers:(kind:function))`
    /// etc.
    fn build_nested_subquery(depth: usize) -> Expr {
        let mut expr = Expr::Condition(Condition {
            field: Field::new("kind"),
            operator: Operator::Equal,
            value: Value::String("function".to_string()),
            span: Span::default(),
        });
        for _ in 0..depth {
            expr = Expr::Condition(Condition {
                field: Field::new("callers"),
                operator: Operator::Equal,
                value: Value::Subquery(Box::new(expr)),
                span: Span::default(),
            });
        }
        expr
    }

    #[test]
    fn test_subquery_depth_at_max_succeeds() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        // Build subquery nested exactly at MAX_SUBQUERY_DEPTH
        let expr = build_nested_subquery(crate::query::types::MAX_SUBQUERY_DEPTH);
        assert!(
            validator.validate(&expr).is_ok(),
            "subquery at exactly MAX_SUBQUERY_DEPTH should be valid"
        );
    }

    #[test]
    fn test_subquery_depth_exceeds_max_fails() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        // Build subquery nested one beyond MAX_SUBQUERY_DEPTH
        let expr = build_nested_subquery(crate::query::types::MAX_SUBQUERY_DEPTH + 1);
        let result = validator.validate(&expr);
        assert!(
            result.is_err(),
            "subquery beyond MAX_SUBQUERY_DEPTH should fail"
        );
        assert!(
            matches!(
                result.unwrap_err(),
                ValidationError::SubqueryDepthExceeded { .. }
            ),
            "error should be SubqueryDepthExceeded"
        );
    }
}
