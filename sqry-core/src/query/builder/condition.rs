//! Condition builder for individual field conditions

use std::borrow::Cow;

use crate::query::{Condition, Field, FieldRegistry, Operator, Span, Value};

/// Builder for individual field conditions.
///
/// Uses `Cow<'static, str>` for field names to avoid allocations when
/// using string literals (common case), while still supporting dynamic strings.
///
/// This is typically constructed internally by `QueryBuilder` methods rather
/// than directly by users.
#[derive(Clone, Debug)]
pub struct ConditionBuilder {
    /// Field name (may be alias - resolved at build time)
    /// Uses Cow to avoid allocation for static string literals
    field: Cow<'static, str>,
    /// Comparison operator
    operator: Operator,
    /// Value to compare against
    value: Value,
}

impl ConditionBuilder {
    /// Create a new condition with a static field name (no allocation)
    ///
    /// This is used by core field methods like `kind()`, `name()`, etc.
    /// where the field name is a compile-time constant.
    #[must_use]
    pub fn new_static(field: &'static str, operator: Operator, value: Value) -> Self {
        Self {
            field: Cow::Borrowed(field),
            operator,
            value,
        }
    }

    /// Create a new condition with a dynamic field name (allocates)
    ///
    /// This is used by generic field methods like `field()` where the
    /// field name is provided at runtime (e.g., for plugin fields).
    pub fn new(field: impl Into<String>, operator: Operator, value: Value) -> Self {
        Self {
            field: Cow::Owned(field.into()),
            operator,
            value,
        }
    }

    /// Get the field name as a string slice
    #[must_use]
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Get the operator
    #[must_use]
    pub fn operator(&self) -> &Operator {
        &self.operator
    }

    /// Get the value
    #[must_use]
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// Convert to Condition, resolving field alias via registry
    ///
    /// The registry is used to resolve field aliases (e.g., "file" -> "path",
    /// "language" -> "lang") to their canonical names.
    #[must_use]
    pub fn into_condition(self, registry: &FieldRegistry) -> Condition {
        // Resolve alias to canonical name (e.g., "file" -> "path", "language" -> "lang")
        let field = self.field;
        let canonical_name = match registry.resolve_canonical(field.as_ref()) {
            Some(canonical) => canonical.to_string(),
            None => field.into_owned(),
        };

        Condition {
            field: Field::new(canonical_name),
            operator: self.operator,
            value: self.value,
            span: Span::synthetic(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::FieldRegistry;

    #[test]
    fn test_new_static() {
        let cond = ConditionBuilder::new_static(
            "kind",
            Operator::Equal,
            Value::String("function".to_string()),
        );
        assert_eq!(cond.field(), "kind");
        assert_eq!(cond.operator(), &Operator::Equal);
        assert!(matches!(cond.value(), Value::String(s) if s == "function"));
    }

    #[test]
    fn test_new_dynamic() {
        let field_name = String::from("custom_field");
        let cond = ConditionBuilder::new(
            field_name,
            Operator::Regex,
            Value::String("pattern".to_string()),
        );
        assert_eq!(cond.field(), "custom_field");
        assert_eq!(cond.operator(), &Operator::Regex);
    }

    #[test]
    fn test_into_condition_canonical() {
        let registry = FieldRegistry::with_core_fields();
        let cond = ConditionBuilder::new_static(
            "kind",
            Operator::Equal,
            Value::String("function".to_string()),
        );
        let condition = cond.into_condition(&registry);
        assert_eq!(condition.field.as_str(), "kind");
        assert!(condition.span.is_synthetic());
    }

    #[test]
    fn test_into_condition_alias_resolution() {
        let registry = FieldRegistry::with_core_fields();

        // "file" should resolve to "path"
        let cond = ConditionBuilder::new_static(
            "file",
            Operator::Equal,
            Value::String("src/main.rs".to_string()),
        );
        let condition = cond.into_condition(&registry);
        assert_eq!(condition.field.as_str(), "path");

        // "language" should resolve to "lang"
        let cond = ConditionBuilder::new_static(
            "language",
            Operator::Equal,
            Value::String("rust".to_string()),
        );
        let condition = cond.into_condition(&registry);
        assert_eq!(condition.field.as_str(), "lang");
    }

    #[test]
    fn test_into_condition_unknown_field_kept() {
        let registry = FieldRegistry::with_core_fields();

        // Unknown field names are kept as-is (validation happens separately)
        let cond = ConditionBuilder::new(
            "unknown_field",
            Operator::Equal,
            Value::String("value".to_string()),
        );
        let condition = cond.into_condition(&registry);
        assert_eq!(condition.field.as_str(), "unknown_field");
    }

    #[test]
    fn test_clone() {
        let cond = ConditionBuilder::new_static(
            "name",
            Operator::Regex,
            Value::String("test.*".to_string()),
        );
        let cloned = cond.clone();
        assert_eq!(cloned.field(), "name");
        assert_eq!(cloned.operator(), &Operator::Regex);
    }
}
