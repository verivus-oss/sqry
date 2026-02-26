//! Query optimizer for efficient execution
//!
//! This module provides query optimization through clause reordering and AST transformations
//! to maximize index usage and minimize evaluation cost.
//!
//! # Optimization Strategies
//!
//! 1. **Flattening**: Collapse nested AND/OR operators
//!    - `AND(AND(A, B), C)` → `AND(A, B, C)`
//!    - `OR(OR(A, B), C)` → `OR(A, B, C)`
//!
//! 2. **Clause Reordering**: Order AND clauses for optimal execution
//!    - Indexed fields first (can use index lookups)
//!    - Within indexed fields, order by selectivity (most selective first)
//!    - Non-indexed fields last (require full scans)
//!
//! 3. **Selectivity Estimation**: Heuristic estimates for field selectivity
//!    - `kind`: 0.1 (10%) - most selective indexed field
//!    - `name`: 0.01 (1%) - very selective
//!    - `path`: 0.3 (30%)
//!    - `lang`: 0.4 (40%)
//!    - `text`: 0.5 (50%) - least selective, not indexed
//!
//! # Example
//!
//! ```ignore
//! use sqry_core::query::optimizer::Optimizer;
//! use sqry_core::query::registry::FieldRegistry;
//!
//! let registry = FieldRegistry::with_core_fields();
//! let optimizer = Optimizer::new(registry);
//!
//! // Optimize: text:TODO AND kind:function
//! // Result:   kind:function AND text:TODO (indexed first)
//! let optimized = optimizer.optimize(ast);
//! ```

use super::registry::FieldRegistry;
use super::types::{Expr, Query};

/// Query optimizer that reorders clauses for efficient execution
///
/// The optimizer uses the field registry to determine which fields are indexed
/// and estimates selectivity to reorder clauses optimally.
#[derive(Debug)]
pub struct Optimizer {
    /// Field registry for index and type information
    registry: FieldRegistry,
}

impl Optimizer {
    /// Create a new optimizer with the given field registry
    #[must_use]
    pub fn new(registry: FieldRegistry) -> Self {
        Self { registry }
    }

    /// Optimize a complete query
    ///
    /// Applies all optimization passes to the query's root expression.
    #[must_use]
    pub fn optimize_query(&self, query: Query) -> Query {
        Query {
            root: self.optimize(query.root),
            span: query.span,
        }
    }

    /// Optimize an expression tree
    ///
    /// Applies the following optimizations recursively:
    /// 1. Flatten nested AND/OR operators
    /// 2. Reorder AND clauses by index availability and selectivity
    /// 3. Recursively optimize child expressions
    #[must_use]
    pub fn optimize(&self, expr: Expr) -> Expr {
        match expr {
            Expr::And(operands) => {
                // First, recursively optimize each operand
                let optimized: Vec<Expr> = operands.into_iter().map(|e| self.optimize(e)).collect();

                // Flatten nested ANDs
                let flattened = self.flatten_and(optimized);

                // Reorder for optimal execution
                self.reorder_and(flattened)
            }
            Expr::Or(operands) => {
                // First, recursively optimize each operand
                let optimized: Vec<Expr> = operands.into_iter().map(|e| self.optimize(e)).collect();

                // Flatten nested ORs
                let flattened = self.flatten_or(optimized);

                // Don't reorder OR clauses (all branches must be evaluated)
                Expr::Or(flattened)
            }
            Expr::Not(operand) => {
                // Recursively optimize the negated expression
                Expr::Not(Box::new(self.optimize(*operand)))
            }
            Expr::Condition(_) => {
                // Conditions are leaf nodes, no optimization needed
                expr
            }
            Expr::Join(join) => {
                // Optimize both sides of the join independently
                Expr::Join(crate::query::types::JoinExpr {
                    left: Box::new(self.optimize(*join.left)),
                    edge: join.edge,
                    right: Box::new(self.optimize(*join.right)),
                    span: join.span,
                })
            }
        }
    }

    /// Flatten nested AND operators
    ///
    /// Transforms `AND(AND(A, B), C)` into `AND(A, B, C)`.
    /// This enables more effective reordering across all AND clauses.
    #[allow(clippy::only_used_in_recursion)]
    fn flatten_and(&self, operands: Vec<Expr>) -> Vec<Expr> {
        let mut result = Vec::new();

        for operand in operands {
            match operand {
                Expr::And(nested) => {
                    // Recursively flatten nested ANDs
                    result.extend(self.flatten_and(nested));
                }
                other => {
                    result.push(other);
                }
            }
        }

        result
    }

    /// Flatten nested OR operators
    ///
    /// Transforms `OR(OR(A, B), C)` into `OR(A, B, C)`.
    #[allow(clippy::only_used_in_recursion)]
    fn flatten_or(&self, operands: Vec<Expr>) -> Vec<Expr> {
        let mut result = Vec::new();

        for operand in operands {
            match operand {
                Expr::Or(nested) => {
                    // Recursively flatten nested ORs
                    result.extend(self.flatten_or(nested));
                }
                other => {
                    result.push(other);
                }
            }
        }

        result
    }

    /// Reorder AND clauses for optimal execution
    ///
    /// Ordering strategy:
    /// 1. Indexed fields first (can use index lookups)
    /// 2. Within indexed fields, order by selectivity (most selective first)
    /// 3. Non-indexed fields last (require full scans)
    /// 4. Non-condition expressions (AND, OR, NOT) maintain relative order at the end
    fn reorder_and(&self, operands: Vec<Expr>) -> Expr {
        if operands.is_empty() {
            // Edge case: empty AND (shouldn't happen in valid queries)
            return Expr::And(vec![]);
        }

        if operands.len() == 1 {
            // Single operand: return it directly (no need for AND wrapper)
            return operands.into_iter().next().unwrap();
        }

        // Separate conditions from complex expressions
        let mut conditions = Vec::new();
        let mut complex_exprs = Vec::new();

        for operand in operands {
            match &operand {
                Expr::Condition(_) => conditions.push(operand),
                _ => complex_exprs.push(operand),
            }
        }

        // Sort conditions by (indexed, selectivity)
        conditions.sort_by(|a, b| {
            let (a_indexed, a_selectivity) = self.analyze_operand(a);
            let (b_indexed, b_selectivity) = self.analyze_operand(b);

            // First, compare by index availability (indexed first)
            match (a_indexed, b_indexed) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    // If both indexed or both not indexed, compare by selectivity
                    // Lower selectivity = more selective = should come first
                    a_selectivity
                        .partial_cmp(&b_selectivity)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            }
        });

        // Combine: conditions (optimally ordered) + complex expressions
        let mut result = conditions;
        result.extend(complex_exprs);

        Expr::And(result)
    }

    /// Analyze an operand to determine index availability and selectivity
    ///
    /// Returns: (`is_indexed`, `selectivity_estimate`)
    /// - `is_indexed`: true if the field can use an index
    /// - selectivity: estimated fraction of results (lower is more selective)
    fn analyze_operand(&self, operand: &Expr) -> (bool, f64) {
        match operand {
            Expr::Condition(condition) => {
                let field_name = condition.field.as_str();

                // Check if field is indexed
                let is_indexed = self
                    .registry
                    .get(field_name)
                    .is_some_and(|desc| desc.indexed);

                // Estimate selectivity
                let selectivity = Self::estimate_selectivity(field_name);

                (is_indexed, selectivity)
            }
            _ => {
                // Complex expressions (AND, OR, NOT) are not directly indexed
                // and have unknown selectivity (assume high cost)
                (false, 0.5)
            }
        }
    }

    /// Estimate selectivity for a field
    ///
    /// Returns the estimated fraction of symbols that will match (0.0 to 1.0).
    /// Lower values are more selective (filter out more results).
    ///
    /// # Heuristic Estimates
    ///
    /// - `kind`: 0.1 (10%) - most selective indexed field
    /// - `name`: 0.01 (1%) - very selective (unique names)
    /// - `path`: 0.3 (30%) - moderately selective
    /// - `lang`: 0.4 (40%) - less selective (few languages)
    /// - `scope`: 0.35 (35%) - moderately selective
    /// - `text`: 0.5 (50%) - least selective, expensive full scan
    /// - Unknown fields: 0.45 (45%) - default moderate selectivity
    fn estimate_selectivity(field: &str) -> f64 {
        match field {
            "kind" => 0.1,   // 10% - very selective
            "name" => 0.01,  // 1% - most selective
            "path" => 0.3,   // 30%
            "lang" => 0.4,   // 40%
            "scope" => 0.35, // 35%
            "text" => 0.5,   // 50% - least selective, not indexed
            _ => 0.45,       // Default for unknown/plugin fields
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::types::{Condition, Field, Operator, Span, Value};
    use approx::assert_abs_diff_eq;

    fn make_condition(field: &str, value: &str) -> Expr {
        Expr::Condition(Condition {
            field: Field::new(field),
            operator: Operator::Equal,
            value: Value::String(value.to_string()),
            span: Span::default(),
        })
    }

    #[test]
    fn test_optimize_single_condition() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let expr = make_condition("kind", "function");
        let rewritten_expr = optimizer.optimize(expr.clone());

        // Single condition should remain unchanged
        assert_eq!(rewritten_expr, expr);
    }

    #[test]
    fn test_flatten_nested_and() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // AND(AND(A, B), C)
        let a = make_condition("kind", "function");
        let b = make_condition("name", "foo");
        let c = make_condition("lang", "rust");

        let nested_and = Expr::And(vec![Expr::And(vec![a.clone(), b.clone()]), c.clone()]);

        let rewritten_expr = optimizer.optimize(nested_and);

        // Should flatten to AND(A, B, C) and reorder
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 3);
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_flatten_deeply_nested_and() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // AND(AND(AND(A, B), C), D)
        let a = make_condition("kind", "function");
        let b = make_condition("name", "foo");
        let c = make_condition("lang", "rust");
        let d = make_condition("path", "src/main.rs");

        let deeply_nested = Expr::And(vec![
            Expr::And(vec![Expr::And(vec![a.clone(), b.clone()]), c.clone()]),
            d.clone(),
        ]);

        let rewritten_expr = optimizer.optimize(deeply_nested);

        // Should flatten to AND(A, B, C, D)
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 4);
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_flatten_nested_or() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // OR(OR(A, B), C)
        let a = make_condition("kind", "function");
        let b = make_condition("kind", "method");
        let c = make_condition("kind", "class");

        let nested_or = Expr::Or(vec![Expr::Or(vec![a.clone(), b.clone()]), c.clone()]);

        let rewritten_expr = optimizer.optimize(nested_or);

        // Should flatten to OR(A, B, C)
        match rewritten_expr {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 3);
            }
            _ => panic!("Expected Or expression"),
        }
    }

    #[test]
    fn test_reorder_for_index() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // text (NOT indexed) AND kind (indexed)
        let text_cond = make_condition("text", "TODO");
        let kind_cond = make_condition("kind", "function");

        let expr = Expr::And(vec![text_cond.clone(), kind_cond.clone()]);
        let rewritten_expr = optimizer.optimize(expr);

        // kind should be first (indexed)
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);
                // First operand should be kind (indexed)
                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "kind");
                    }
                    _ => panic!("Expected Condition"),
                }
                // Second operand should be text (not indexed)
                match &operands[1] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "text");
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_reorder_by_selectivity() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // lang (40%) AND name (1%)
        // Both indexed, but name is more selective
        let lang_cond = make_condition("lang", "rust");
        let name_cond = make_condition("name", "foo");

        let expr = Expr::And(vec![lang_cond.clone(), name_cond.clone()]);
        let rewritten_expr = optimizer.optimize(expr);

        // name should be first (more selective)
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);
                // First operand should be name (more selective)
                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "name");
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_reorder_mixed_indexed_and_selectivity() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // path (indexed, 30%) AND kind (indexed, 10%) AND text (not indexed, 50%)
        let path_cond = make_condition("path", "src/**/*.rs");
        let kind_cond = make_condition("kind", "function");
        let text_cond = make_condition("text", "TODO");

        let expr = Expr::And(vec![
            path_cond.clone(),
            kind_cond.clone(),
            text_cond.clone(),
        ]);
        let rewritten_expr = optimizer.optimize(expr);

        // Expected order: kind (indexed, 10%), path (indexed, 30%), text (not indexed, 50%)
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 3);

                // First: kind (indexed, most selective)
                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "kind");
                    }
                    _ => panic!("Expected Condition"),
                }

                // Second: path (indexed, less selective)
                match &operands[1] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "path");
                    }
                    _ => panic!("Expected Condition"),
                }

                // Third: text (not indexed)
                match &operands[2] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "text");
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_preserve_or_order() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // OR clauses should not be reordered (all branches must be evaluated)
        let text_cond = make_condition("text", "TODO");
        let kind_cond = make_condition("kind", "function");

        let expr = Expr::Or(vec![text_cond.clone(), kind_cond.clone()]);
        let rewritten_expr = optimizer.optimize(expr);

        // Order should be preserved
        match rewritten_expr {
            Expr::Or(operands) => {
                assert_eq!(operands.len(), 2);
                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "text");
                    }
                    _ => panic!("Expected Condition"),
                }
            }
            _ => panic!("Expected Or expression"),
        }
    }

    #[test]
    fn test_optimize_not() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let kind_cond = make_condition("kind", "function");
        let expr = Expr::Not(Box::new(kind_cond.clone()));

        let rewritten_expr = optimizer.optimize(expr);

        // NOT should be preserved and inner expression optimized
        match rewritten_expr {
            Expr::Not(inner) => {
                assert_eq!(*inner, kind_cond);
            }
            _ => panic!("Expected Not expression"),
        }
    }

    #[test]
    fn test_optimize_complex_nested() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // (text:TODO OR name:foo) AND kind:function
        let text_cond = make_condition("text", "TODO");
        let name_cond = make_condition("name", "foo");
        let kind_cond = make_condition("kind", "function");

        let or_expr = Expr::Or(vec![text_cond.clone(), name_cond.clone()]);
        let expr = Expr::And(vec![or_expr.clone(), kind_cond.clone()]);

        let rewritten_expr = optimizer.optimize(expr);

        // kind should be first (indexed), OR should be second
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 2);

                // First should be kind
                match &operands[0] {
                    Expr::Condition(cond) => {
                        assert_eq!(cond.field.as_str(), "kind");
                    }
                    _ => panic!("Expected Condition"),
                }

                // Second should be OR
                match &operands[1] {
                    Expr::Or(_) => {
                        // Correct
                    }
                    _ => panic!("Expected Or expression"),
                }
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_optimize_query() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let text_cond = make_condition("text", "TODO");
        let kind_cond = make_condition("kind", "function");

        let query = Query {
            root: Expr::And(vec![text_cond, kind_cond]),
            span: Span::default(),
        };

        let rewritten_query = optimizer.optimize_query(query);

        // Should reorder to kind first
        match rewritten_query.root {
            Expr::And(operands) => match &operands[0] {
                Expr::Condition(cond) => {
                    assert_eq!(cond.field.as_str(), "kind");
                }
                _ => panic!("Expected Condition"),
            },
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_analyze_operand_indexed_field() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let kind_cond = make_condition("kind", "function");
        let (indexed, selectivity) = optimizer.analyze_operand(&kind_cond);

        assert!(indexed);
        assert_abs_diff_eq!(selectivity, 0.1, epsilon = 1e-10); // kind selectivity
    }

    #[test]
    fn test_analyze_operand_non_indexed_field() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let text_cond = make_condition("text", "TODO");
        let (indexed, selectivity) = optimizer.analyze_operand(&text_cond);

        assert!(!indexed);
        assert_abs_diff_eq!(selectivity, 0.5, epsilon = 1e-10); // text selectivity
    }

    #[test]
    fn test_analyze_operand_complex_expression() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let a = make_condition("kind", "function");
        let b = make_condition("name", "foo");
        let or_expr = Expr::Or(vec![a, b]);

        let (indexed, selectivity) = optimizer.analyze_operand(&or_expr);

        // Complex expressions are not directly indexed
        assert!(!indexed);
        assert_abs_diff_eq!(selectivity, 0.5, epsilon = 1e-10); // Default for complex expressions
    }

    #[test]
    fn test_estimate_selectivity_known_fields() {
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("kind"),
            0.1,
            epsilon = 1e-10
        );
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("name"),
            0.01,
            epsilon = 1e-10
        );
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("path"),
            0.3,
            epsilon = 1e-10
        );
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("lang"),
            0.4,
            epsilon = 1e-10
        );
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("scope"),
            0.35,
            epsilon = 1e-10
        );
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("text"),
            0.5,
            epsilon = 1e-10
        );
    }

    #[test]
    fn test_estimate_selectivity_unknown_field() {
        // Unknown field should have default selectivity
        assert_abs_diff_eq!(
            Optimizer::estimate_selectivity("unknown_field"),
            0.45,
            epsilon = 1e-10
        );
    }

    #[test]
    fn test_single_and_operand_unwrapped() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        // AND with single operand should return the operand directly
        let kind_cond = make_condition("kind", "function");
        let expr = Expr::And(vec![kind_cond.clone()]);

        let rewritten_expr = optimizer.optimize(expr);

        // Should return the condition directly, not wrapped in AND
        assert_eq!(rewritten_expr, kind_cond);
    }

    #[test]
    fn test_empty_and() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let expr = Expr::And(vec![]);
        let rewritten_expr = optimizer.optimize(expr);

        // Empty AND should remain empty AND
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 0);
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_flatten_multiple_and_levels() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let a = make_condition("kind", "function");
        let b = make_condition("name", "foo");
        let c = make_condition("lang", "rust");
        let d = make_condition("path", "src/main.rs");

        // (A AND B) AND (C AND D)
        let expr = Expr::And(vec![
            Expr::And(vec![a.clone(), b.clone()]),
            Expr::And(vec![c.clone(), d.clone()]),
        ]);

        let rewritten_expr = optimizer.optimize(expr);

        // Should flatten to AND(A, B, C, D) and reorder
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 4, "Should flatten to 4 operands");

                // Verify all expected fields are present
                let field_names: Vec<_> = operands
                    .iter()
                    .filter_map(|op| match op {
                        Expr::Condition(cond) => Some(cond.field.as_str()),
                        _ => None,
                    })
                    .collect();

                assert_eq!(field_names.len(), 4);
                assert!(field_names.contains(&"kind"));
                assert!(field_names.contains(&"name"));
                assert!(field_names.contains(&"lang"));
                assert!(field_names.contains(&"path"));
            }
            _ => panic!("Expected And expression"),
        }
    }

    #[test]
    fn test_flatten_with_mixed_nesting() {
        let registry = FieldRegistry::with_core_fields();
        let optimizer = Optimizer::new(registry);

        let a = make_condition("kind", "function");
        let b = make_condition("name", "foo");
        let c = make_condition("lang", "rust");
        let d = make_condition("path", "src/main.rs");

        // A AND (B AND (C AND D))
        let expr = Expr::And(vec![
            a.clone(),
            Expr::And(vec![b.clone(), Expr::And(vec![c.clone(), d.clone()])]),
        ]);

        let rewritten_expr = optimizer.optimize(expr);

        // Expected: AND(A, B, C, D) flattened and reordered
        match rewritten_expr {
            Expr::And(operands) => {
                assert_eq!(operands.len(), 4, "Should flatten to 4 operands");

                // Verify all expected fields are present
                let field_names: Vec<_> = operands
                    .iter()
                    .filter_map(|op| match op {
                        Expr::Condition(cond) => Some(cond.field.as_str()),
                        _ => None,
                    })
                    .collect();

                assert_eq!(field_names.len(), 4);
                assert!(field_names.contains(&"kind"));
                assert!(field_names.contains(&"name"));
                assert!(field_names.contains(&"lang"));
                assert!(field_names.contains(&"path"));
            }
            _ => panic!("Expected And expression"),
        }
    }
}
