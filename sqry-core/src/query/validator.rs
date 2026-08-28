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
    // Phase A C indirect-call precision (U18.1) — fuzzy-correct common typos
    // like `address_take:`, `resolve_via:`, `callsite_promiscous:` so users on
    // the `mcp__sqry__semantic_search` surface get the same suggestion
    // experience the planner-surface (`sqry_query`) already provides via U14.
    "address_taken",
    "resolved_via",
    "callsite_promiscuous",
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

    /// Canonicalize a closed-vocabulary field's VALUE, mirroring the field-name
    /// canonicalization above.
    ///
    /// #522 made `file:` a true alias of `path:` by resolving the field name
    /// once here, at the single chokepoint every query passes through before
    /// validation and execution. The same defect existed one level down: the
    /// registry knows `lang` accepts `ts`, but the executor compares canonical
    /// names, so `lang:ts` validated cleanly and then silently matched nothing.
    ///
    /// Resolving the value here fixes every sqry-core-parser surface at once,
    /// and it must happen BEFORE `validate_enum_value`, which is exact and
    /// case-sensitive. An unrecognized value is passed through untouched so
    /// that enum validation reports it rather than this silently swallowing it.
    fn normalize_value(canonical_field: &str, value: Value) -> Value {
        match (canonical_field, &value) {
            ("lang", Value::String(raw)) => crate::graph::node::Language::from_id(raw).map_or_else(
                || value.clone(),
                |lang| Value::String(lang.canonical_name().to_string()),
            ),
            _ => value,
        }
    }

    fn normalize_condition(&self, condition: &Condition) -> Result<Condition, ValidationError> {
        // Normalize a nested subquery value first, recursively, so alias
        // resolution reaches every field inside a relation subquery, e.g.
        // `callers:(file:main.rs)`, and not just the outer condition.
        // `validate_node_with_depth` and `resolve_variables` both descend into
        // `Value::Subquery`, so normalization must too; otherwise a `file:`
        // alias nested in a subquery survives to the executor unresolved,
        // hits graph_eval's `_ => Ok(false)` (no `file` arm), and silently
        // matches nothing. Recursing through `normalize_expr` covers boolean
        // and negation wrappers inside the subquery as well (issue #513).
        let normalized_value = match &condition.value {
            Value::Subquery(inner) => Value::Subquery(Box::new(self.normalize_expr(inner)?)),
            other => other.clone(),
        };

        // Fast path: known field, either a canonical name or a registered alias.
        //
        // Resolve any alias to its canonical field name here. Downstream
        // evaluators match on the canonical field (e.g. `path`), so an alias
        // like `file` (registered as `file` -> `path`) that survives to the
        // executor has no match arm and silently returns zero results even
        // though validation passed. Rewriting the field to its canonical form
        // makes `file:` a true alias of `path:` end to end (issue #513).
        let field_name = condition.field.as_str();
        let canonical = self
            .registry
            .resolve_canonical(field_name)
            .map(std::string::ToString::to_string);
        if let Some(canonical) = canonical {
            let mut resolved = condition.clone();
            // Normalize the value before moving `canonical` into the field.
            resolved.value = Self::normalize_value(&canonical, normalized_value);
            if canonical != field_name {
                resolved.field = Field::new(canonical);
            }
            return Ok(resolved);
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
                corrected.value = normalized_value;
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

    /// Issue #513: `file:` is advertised as an alias of `path:` but the
    /// executor only has a `path` match arm, so an unresolved `file` field
    /// silently matched nothing. `normalize_expr` must rewrite the alias to
    /// its canonical field name so both spell out to the same condition.
    #[test]
    fn test_normalize_resolves_file_alias_to_path() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let file_expr = Expr::Condition(Condition {
            field: Field::new("file"),
            operator: Operator::Equal,
            value: Value::String("crates/**".to_string()),
            span: Span::default(),
        });
        let normalized = validator.normalize_expr(&file_expr).unwrap();
        let Expr::Condition(cond) = normalized else {
            panic!("expected condition");
        };
        assert_eq!(cond.field.as_str(), "path");
        assert!(matches!(cond.value, Value::String(ref s) if s == "crates/**"));
        assert_eq!(cond.operator, Operator::Equal);
    }

    /// `file:X` and `path:X` must normalize to identical conditions so the two
    /// spellings return the same matches end to end (issue #513).
    #[test]
    fn test_normalize_file_and_path_are_equivalent() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let make = |field: &str| {
            Expr::Condition(Condition {
                field: Field::new(field),
                operator: Operator::Equal,
                value: Value::String("src/**/*.rs".to_string()),
                span: Span::default(),
            })
        };

        let from_file = validator.normalize_expr(&make("file")).unwrap();
        let from_path = validator.normalize_expr(&make("path")).unwrap();

        let (Expr::Condition(file_cond), Expr::Condition(path_cond)) = (from_file, from_path)
        else {
            panic!("expected conditions");
        };
        assert_eq!(file_cond.field.as_str(), path_cond.field.as_str());
        assert_eq!(file_cond.field.as_str(), "path");
        assert_eq!(file_cond.operator, path_cond.operator);
        assert!(matches!((&file_cond.value, &path_cond.value),
                (Value::String(a), Value::String(b)) if a == b));

        // Both must still pass validation after normalization.
        assert!(validator.validate(&Expr::Condition(file_cond)).is_ok());
        assert!(validator.validate(&Expr::Condition(path_cond)).is_ok());
    }

    /// The `language` -> `lang` alias must canonicalize the same way, and a
    /// nested alias inside a boolean expression must be rewritten too.
    #[test]
    fn test_normalize_language_alias_and_nested() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let expr = Expr::And(vec![
            Expr::Condition(Condition {
                field: Field::new("kind"),
                operator: Operator::Equal,
                value: Value::String("struct".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("file"),
                operator: Operator::Equal,
                value: Value::String("crates/**".to_string()),
                span: Span::default(),
            }),
            Expr::Condition(Condition {
                field: Field::new("language"),
                operator: Operator::Equal,
                value: Value::String("rust".to_string()),
                span: Span::default(),
            }),
        ]);

        let Expr::And(operands) = validator.normalize_expr(&expr).unwrap() else {
            panic!("expected And");
        };
        let fields: Vec<&str> = operands
            .iter()
            .map(|op| match op {
                Expr::Condition(c) => c.field.as_str(),
                _ => panic!("expected condition"),
            })
            .collect();
        assert_eq!(fields, vec!["kind", "path", "lang"]);
    }

    /// Issue #513 (subquery regression): a `file:` alias inside a relation
    /// subquery, e.g. `callers:(file:main.rs)`, must be canonicalized to
    /// `path` too. Normalization used to rewrite only the outer condition and
    /// leave `Value::Subquery` fields untouched, so nested `file:` reached the
    /// executor unresolved and matched nothing.
    #[test]
    fn test_normalize_resolves_file_alias_inside_subquery() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let relation_with = |inner_field: &str| {
            Expr::Condition(Condition {
                field: Field::new("callers"),
                operator: Operator::Equal,
                value: Value::Subquery(Box::new(Expr::Condition(Condition {
                    field: Field::new(inner_field),
                    operator: Operator::Equal,
                    value: Value::String("main.rs".to_string()),
                    span: Span::default(),
                }))),
                span: Span::default(),
            })
        };

        let from_file = validator.normalize_expr(&relation_with("file")).unwrap();
        let from_path = validator.normalize_expr(&relation_with("path")).unwrap();

        // The inner subquery field must be canonicalized to `path`, and the two
        // spellings must produce an identical normalized AST.
        assert_eq!(from_file, from_path);

        let Expr::Condition(cond) = from_file else {
            panic!("expected condition");
        };
        let Value::Subquery(inner) = cond.value else {
            panic!("expected subquery value");
        };
        let Expr::Condition(inner_cond) = *inner else {
            panic!("expected inner condition");
        };
        assert_eq!(inner_cond.field.as_str(), "path");
    }

    /// The recursion must also reach fields nested inside a boolean wrapper
    /// within a subquery, e.g. `callers:(file:a.rs OR file:b.rs)`.
    #[test]
    fn test_normalize_resolves_file_alias_in_nested_boolean_subquery() {
        let registry = FieldRegistry::with_core_fields();
        let validator = Validator::new(registry);

        let make_leaf = |value: &str| {
            Expr::Condition(Condition {
                field: Field::new("file"),
                operator: Operator::Equal,
                value: Value::String(value.to_string()),
                span: Span::default(),
            })
        };
        let expr = Expr::Condition(Condition {
            field: Field::new("callers"),
            operator: Operator::Equal,
            value: Value::Subquery(Box::new(Expr::Or(vec![
                make_leaf("a.rs"),
                make_leaf("b.rs"),
            ]))),
            span: Span::default(),
        });

        let Expr::Condition(cond) = validator.normalize_expr(&expr).unwrap() else {
            panic!("expected condition");
        };
        let Value::Subquery(inner) = cond.value else {
            panic!("expected subquery value");
        };
        let Expr::Or(operands) = *inner else {
            panic!("expected Or inside subquery");
        };
        for op in operands {
            let Expr::Condition(leaf) = op else {
                panic!("expected condition leaf");
            };
            assert_eq!(leaf.field.as_str(), "path");
        }
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

    // ── issue #714: the chokepoint canonicalizes VALUES, not just field names ──

    fn lang_condition(value: &str) -> Condition {
        Condition {
            field: Field::new("lang"),
            operator: Operator::Equal,
            value: Value::String(value.to_string()),
            span: Span::default(),
        }
    }

    #[test]
    fn normalize_canonicalizes_language_aliases() {
        let validator = Validator::new(FieldRegistry::with_core_fields());

        // #522 made `file:` a true alias of `path:` by resolving the field
        // NAME here. The same defect lived one level down in the value: the
        // registry knows `ts` is TypeScript, but the executor compares
        // canonical names, so `lang:ts` validated and matched nothing.
        for spelling in ["ts", "TS", "  TypeScript  ", "typescript"] {
            let normalized = validator
                .normalize_condition(&lang_condition(spelling))
                .expect("alias must validate");
            assert_eq!(
                normalized.value,
                Value::String("typescript".to_string()),
                "{spelling} should canonicalize to typescript"
            );
        }

        // The `language:` field alias and a value alias must resolve together.
        let cond = Condition {
            field: Field::new("language"),
            operator: Operator::Equal,
            value: Value::String("rs".to_string()),
            span: Span::default(),
        };
        let normalized = validator.normalize_condition(&cond).expect("must validate");
        assert_eq!(normalized.field.as_str(), "lang");
        assert_eq!(normalized.value, Value::String("rust".to_string()));
    }

    #[test]
    fn normalize_leaves_other_fields_values_alone() {
        let validator = Validator::new(FieldRegistry::with_core_fields());
        let cond = Condition {
            field: Field::new("name"),
            operator: Operator::Equal,
            value: Value::String("ts".to_string()),
            span: Span::default(),
        };
        let normalized = validator.normalize_condition(&cond).expect("must validate");
        // `ts` is a language alias but this is a symbol name, not a language.
        assert_eq!(normalized.value, Value::String("ts".to_string()));
    }

    #[test]
    fn unknown_language_is_a_validation_error_not_a_silent_zero() {
        let validator = Validator::new(FieldRegistry::with_core_fields());
        let expr = Expr::Condition(lang_condition("bogus"));

        let normalized = validator
            .normalize_expr(&expr)
            .expect("normalize passes an unknown value through untouched");
        let err = validator
            .validate(&normalized)
            .expect_err("an unrecognized language must fail validation");

        match err {
            ValidationError::InvalidEnumValue {
                ref field,
                ref value,
                ref valid_values,
                ..
            } => {
                assert_eq!(field, "lang");
                assert_eq!(value, "bogus");
                assert!(valid_values.contains(&"typescript"));
            }
            other => panic!("expected InvalidEnumValue, got {other:?}"),
        }
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
