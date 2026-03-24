//! Lifetime constraint extraction for Rust.
//!
//! This module extracts lifetime parameters and their constraints from Rust AST
//! nodes, creating `NodeKind::Lifetime` nodes and `LifetimeConstraint` edges.
//!
//! # Coverage
//!
//! | Pattern | AST-only | With RA | LifetimeConstraintKind |
//! |---------|----------|---------|------------------------|
//! | `'a: 'b` | Yes | Yes | Outlives |
//! | `T: 'a` | Yes | Yes | TypeBound |
//! | `&'a T` | Yes | Yes | Reference |
//! | `'static` | Yes | Yes | Static |
//! | `for<'a> T: Trait<'a>` | Yes | Yes | HigherRanked |
//! | `dyn Trait + 'a` | Yes | Yes | TraitObject |
//! | `impl Trait + 'a` | Yes | Yes | ImplTrait |
//! | Elided lifetimes | No | Yes | Elided |

use crate::confidence::ConfidenceTracker;
use sqry_core::graph::Span;
use sqry_core::graph::node::Position;
use sqry_core::graph::unified::edge::kind::LifetimeConstraintKind;
use std::collections::HashMap;
use tree_sitter::Node;

/// A lifetime node with its qualified name.
#[derive(Debug, Clone)]
pub struct LifetimeNode {
    /// The lifetime name (e.g., `'a`, `'static`)
    pub name: String,
    /// The qualified name including owner (e.g., `foo::'a`)
    pub qualified_name: String,
    /// The span where the lifetime is defined
    pub span: Option<Span>,
}

/// A lifetime constraint edge.
#[derive(Debug, Clone)]
pub struct LifetimeEdge {
    /// Source lifetime or type qualified name
    pub source_qualified: String,
    /// Target lifetime qualified name
    pub target_qualified: String,
    /// The kind of constraint
    pub constraint_kind: LifetimeConstraintKind,
    /// The span where the constraint is defined
    pub span: Option<Span>,
}

/// Result of lifetime extraction.
#[derive(Debug, Default)]
pub struct LifetimeExtractionResult {
    /// Lifetime nodes to create
    pub nodes: Vec<LifetimeNode>,
    /// Lifetime constraint edges
    pub edges: Vec<LifetimeEdge>,
}

impl LifetimeExtractionResult {
    /// Check if any lifetimes were extracted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Merge another result into this one.
    pub fn merge(&mut self, other: Self) {
        self.nodes.extend(other.nodes);
        self.edges.extend(other.edges);
    }
}

/// Extracts lifetime constraints from Rust AST.
///
/// This extractor handles all lifetime patterns that can be determined
/// from the AST. Elided lifetimes require rust-analyzer integration
/// and are not extracted in AST-only mode.
pub struct LifetimeExtractor<'a> {
    /// The source content for text extraction
    content: &'a [u8],
    /// Qualified name of the owner (function, struct, etc.)
    owner_qualified: String,
    /// Confidence tracker for recording limitations
    confidence: &'a mut ConfidenceTracker,
    /// Map of lifetime names to their qualified names
    lifetime_map: HashMap<String, String>,
}

impl<'a> LifetimeExtractor<'a> {
    /// Create a new lifetime extractor.
    pub fn new(
        content: &'a [u8],
        owner_qualified: String,
        confidence: &'a mut ConfidenceTracker,
    ) -> Self {
        Self {
            content,
            owner_qualified,
            confidence,
            lifetime_map: HashMap::new(),
        }
    }

    /// Extract lifetime constraints from a function, struct, trait, or impl item.
    ///
    /// This is the main entry point for lifetime extraction.
    pub fn extract(&mut self, node: Node) -> LifetimeExtractionResult {
        let mut result = LifetimeExtractionResult::default();

        // 1. Extract lifetime parameters from type_parameters
        if let Some(params) = node.child_by_field_name("type_parameters") {
            self.extract_lifetime_parameters(params, &mut result);
        }

        // Track what we've processed to avoid double extraction
        let mut processed_return_type = false;

        // Iterate through children to find where_clause, parameters, and return_type
        // as they may not all be accessible via child_by_field_name
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "where_clause" => {
                    // 2. Extract constraints from where_clause
                    self.extract_from_where_clause(child, &mut result);
                }
                "parameters" => {
                    // 3. Extract lifetime references from parameters
                    self.extract_from_parameters(child, &mut result);
                }
                "return_type" => {
                    // 4. Extract from return_type
                    self.extract_from_return_type(child, &mut result);
                    processed_return_type = true;
                }
                _ => {}
            }
        }

        // 4. Extract from return_type (fallback via field name if not found via iteration)
        if !processed_return_type && let Some(return_type) = node.child_by_field_name("return_type")
        {
            self.extract_from_return_type(return_type, &mut result);
        }

        // 5. Extract from impl type (for impl blocks) - the "for Type" part
        // Handles lifetimes in patterns like `impl<'a> ... for Foo<'a>`
        if let Some(type_node) = node.child_by_field_name("type") {
            self.extract_from_type(type_node, &mut result);
        }

        // 6. Extract from trait field (for impl blocks) - the "Trait<'a>" part
        // Handles lifetimes in patterns like `impl<'a> Trait<'a> for Type`
        if let Some(trait_node) = node.child_by_field_name("trait") {
            self.extract_from_type(trait_node, &mut result);
        }

        result
    }

    /// Extract lifetime parameters from `type_parameters` (generics).
    fn extract_lifetime_parameters(&mut self, params: Node, result: &mut LifetimeExtractionResult) {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            match child.kind() {
                "lifetime_parameter" => {
                    // tree-sitter structure: lifetime_parameter -> lifetime
                    if let Some(lifetime) = child.child_by_field_name("lifetime") {
                        self.create_lifetime_node(lifetime, result);
                    } else {
                        // Fallback: look for lifetime child directly
                        let mut inner_cursor = child.walk();
                        for inner in child.children(&mut inner_cursor) {
                            if inner.kind() == "lifetime" {
                                self.create_lifetime_node(inner, result);
                                break;
                            }
                        }
                    }
                }
                "lifetime" => {
                    // Direct lifetime (fallback for grammar variations)
                    self.create_lifetime_node(child, result);
                }
                "type_parameter" => {
                    // tree-sitter: type_parameter -> type_identifier + trait_bounds
                    // Handles patterns like `T: 'a + Clone`
                    self.extract_type_param_with_bounds(child, result);
                }
                "constrained_type_parameter" => {
                    // Legacy support for constrained_type_parameter
                    self.extract_type_parameter_bounds(child, result);
                }
                _ => {}
            }
        }
    }

    /// Extract lifetime bounds from a `type_parameter` node (e.g., `T: 'a`).
    fn extract_type_param_with_bounds(
        &mut self,
        node: Node,
        result: &mut LifetimeExtractionResult,
    ) {
        // Find the type identifier name
        let type_name = node
            .children(&mut node.walk())
            .find(|c| c.kind() == "type_identifier")
            .map(|n| self.node_text(n))
            .unwrap_or_default();

        if type_name.is_empty() {
            return;
        }

        let type_qualified = format!("{}::{}", self.owner_qualified, type_name);

        // Find trait_bounds child and extract lifetime bounds
        let mut inner_cursor = node.walk();
        for child in node.children(&mut inner_cursor) {
            if child.kind() == "trait_bounds" {
                self.extract_lifetime_bounds_from_trait_bounds(&child, &type_qualified, result);
            }
        }
    }

    /// Extract lifetime bounds from `trait_bounds` (: 'a + 'b).
    fn extract_lifetime_bounds_from_trait_bounds(
        &self,
        bounds: &Node,
        type_qualified: &str,
        result: &mut LifetimeExtractionResult,
    ) {
        let mut cursor = bounds.walk();
        for child in bounds.children(&mut cursor) {
            if child.kind() == "lifetime" {
                let lifetime_name = self.node_text(child);
                if lifetime_name == "'static" {
                    result.edges.push(LifetimeEdge {
                        source_qualified: type_qualified.to_string(),
                        target_qualified: "::static".to_string(),
                        constraint_kind: LifetimeConstraintKind::Static,
                        span: Some(span_from_node(child)),
                    });
                } else if let Some(lifetime_qualified) = self.lifetime_map.get(&lifetime_name) {
                    result.edges.push(LifetimeEdge {
                        source_qualified: type_qualified.to_string(),
                        target_qualified: lifetime_qualified.clone(),
                        constraint_kind: LifetimeConstraintKind::TypeBound,
                        span: Some(span_from_node(child)),
                    });
                } else {
                    // Create unresolved edge with inline qualified name
                    result.edges.push(LifetimeEdge {
                        source_qualified: type_qualified.to_string(),
                        target_qualified: format!("{}::{}", self.owner_qualified, lifetime_name),
                        constraint_kind: LifetimeConstraintKind::TypeBound,
                        span: Some(span_from_node(child)),
                    });
                }
            }
        }
    }

    /// Create a lifetime node from a lifetime AST node.
    fn create_lifetime_node(&mut self, node: Node, result: &mut LifetimeExtractionResult) {
        let name = self.node_text(node);
        if name.is_empty() {
            return;
        }

        let qualified_name = format!("{}::{}", self.owner_qualified, name);
        let span = span_from_node(node);

        // Register in map for constraint resolution
        self.lifetime_map
            .insert(name.clone(), qualified_name.clone());

        result.nodes.push(LifetimeNode {
            name,
            qualified_name,
            span: Some(span),
        });
    }

    /// Extract bounds from a constrained type parameter (e.g., `T: 'a + Clone`).
    fn extract_type_parameter_bounds(&mut self, node: Node, result: &mut LifetimeExtractionResult) {
        let type_name = node
            .child_by_field_name("left")
            .map(|n| self.node_text(n))
            .unwrap_or_default();

        if type_name.is_empty() {
            return;
        }

        let type_qualified = format!("{}::{}", self.owner_qualified, type_name);

        // Look for lifetime bounds in the right side
        if let Some(bounds) = node.child_by_field_name("bounds") {
            self.extract_lifetime_bounds_from_type(bounds, &type_qualified, result);
        }
    }

    /// Extract lifetime bounds for a type (T: 'a).
    fn extract_lifetime_bounds_from_type(
        &self,
        bounds: Node,
        type_qualified: &str,
        result: &mut LifetimeExtractionResult,
    ) {
        let mut cursor = bounds.walk();
        for child in bounds.children(&mut cursor) {
            if child.kind() == "lifetime" {
                let lifetime_name = self.node_text(child);
                if let Some(lifetime_qualified) = self.lifetime_map.get(&lifetime_name) {
                    result.edges.push(LifetimeEdge {
                        source_qualified: type_qualified.to_string(),
                        target_qualified: lifetime_qualified.clone(),
                        constraint_kind: LifetimeConstraintKind::TypeBound,
                        span: Some(span_from_node(child)),
                    });
                } else if lifetime_name == "'static" {
                    result.edges.push(LifetimeEdge {
                        source_qualified: type_qualified.to_string(),
                        target_qualified: "::static".to_string(),
                        constraint_kind: LifetimeConstraintKind::Static,
                        span: Some(span_from_node(child)),
                    });
                }
            }
        }
    }

    /// Extract constraints from a where clause.
    fn extract_from_where_clause(
        &mut self,
        where_clause: Node,
        result: &mut LifetimeExtractionResult,
    ) {
        let mut cursor = where_clause.walk();
        for child in where_clause.children(&mut cursor) {
            match child.kind() {
                "where_predicate" => {
                    self.extract_from_where_predicate(child, result);
                }
                "lifetime_predicate" => {
                    // 'a: 'b pattern
                    self.extract_lifetime_predicate(child, result);
                }
                _ => {}
            }
        }
    }

    /// Extract from a where predicate (type bounds or lifetime outlives).
    fn extract_from_where_predicate(&mut self, node: Node, result: &mut LifetimeExtractionResult) {
        // Look for higher-ranked trait bounds (for<'a> ...)
        if let Some(for_lifetimes) = node.child_by_field_name("higher_ranked_trait_bound") {
            self.extract_hrtb(for_lifetimes, result);
        }

        // Check for lifetime outlives pattern: where_predicate -> lifetime + trait_bounds -> lifetime
        // This handles `'a: 'b` in where clauses
        let mut source_lifetime: Option<(String, Node)> = None;
        let mut trait_bounds_node: Option<Node> = None;

        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "lifetime" => {
                    source_lifetime = Some((self.node_text(child), child));
                }
                "trait_bounds" => {
                    trait_bounds_node = Some(child);
                }
                _ => {}
            }
        }

        // If we found a direct lifetime and trait_bounds, this is an outlives constraint
        if let (Some((source_name, _)), Some(bounds)) = (source_lifetime, trait_bounds_node) {
            let source_qualified = self
                .lifetime_map
                .get(&source_name)
                .cloned()
                .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, source_name));

            // Extract target lifetimes from trait_bounds
            let mut bounds_cursor = bounds.walk();
            for child in bounds.children(&mut bounds_cursor) {
                if child.kind() == "lifetime" {
                    let target_name = self.node_text(child);
                    let target_qualified = if target_name == "'static" {
                        "::static".to_string()
                    } else {
                        self.lifetime_map
                            .get(&target_name)
                            .cloned()
                            .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, target_name))
                    };

                    result.edges.push(LifetimeEdge {
                        source_qualified: source_qualified.clone(),
                        target_qualified,
                        constraint_kind: LifetimeConstraintKind::Outlives,
                        span: Some(span_from_node(child)),
                    });
                }
            }
            return;
        }

        // Extract type bounds (fallback to original logic)
        let type_name = node
            .child_by_field_name("left")
            .map(|n| self.node_text(n))
            .unwrap_or_default();

        if !type_name.is_empty() {
            let type_qualified = format!("{}::{}", self.owner_qualified, type_name);
            if let Some(bounds) = node.child_by_field_name("bounds") {
                self.extract_lifetime_bounds_from_type(bounds, &type_qualified, result);
            }
        }
    }

    /// Extract lifetime outlives predicate ('a: 'b).
    fn extract_lifetime_predicate(&self, node: Node, result: &mut LifetimeExtractionResult) {
        let left = node.child_by_field_name("left").map(|n| self.node_text(n));
        let bounds = node.child_by_field_name("bounds");

        if let (Some(source_name), Some(bounds_node)) = (left, bounds) {
            let source_qualified = self
                .lifetime_map
                .get(&source_name)
                .cloned()
                .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, source_name));

            let mut cursor = bounds_node.walk();
            for child in bounds_node.children(&mut cursor) {
                if child.kind() == "lifetime" {
                    let target_name = self.node_text(child);
                    let target_qualified = if target_name == "'static" {
                        "::static".to_string()
                    } else {
                        self.lifetime_map
                            .get(&target_name)
                            .cloned()
                            .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, target_name))
                    };

                    result.edges.push(LifetimeEdge {
                        source_qualified: source_qualified.clone(),
                        target_qualified,
                        constraint_kind: LifetimeConstraintKind::Outlives,
                        span: Some(span_from_node(child)),
                    });
                }
            }
        }
    }

    /// Extract higher-ranked trait bounds (for<'a> ...).
    fn extract_hrtb(&mut self, node: Node, result: &mut LifetimeExtractionResult) {
        // Create lifetime nodes for HRTB-scoped lifetimes
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "lifetime" {
                let name = self.node_text(child);
                let qualified_name = format!("{}::for::{}", self.owner_qualified, name);
                let span = span_from_node(child);

                // Register for constraint resolution
                self.lifetime_map
                    .insert(name.clone(), qualified_name.clone());

                result.nodes.push(LifetimeNode {
                    name,
                    qualified_name: qualified_name.clone(),
                    span: Some(span),
                });

                // Add HigherRanked constraint edge from owner to lifetime
                result.edges.push(LifetimeEdge {
                    source_qualified: self.owner_qualified.clone(),
                    target_qualified: qualified_name,
                    constraint_kind: LifetimeConstraintKind::HigherRanked,
                    span: Some(span),
                });
            }
        }
    }

    /// Extract lifetime references from function parameters.
    fn extract_from_parameters(&mut self, params: Node, result: &mut LifetimeExtractionResult) {
        let mut cursor = params.walk();
        for child in params.children(&mut cursor) {
            if child.kind() == "parameter"
                && let Some(type_node) = child.child_by_field_name("type")
            {
                self.extract_from_type(type_node, result);
            }
        }
    }

    /// Extract lifetime references from return type.
    fn extract_from_return_type(
        &mut self,
        return_type: Node,
        result: &mut LifetimeExtractionResult,
    ) {
        self.extract_from_type(return_type, result);
    }

    /// Extract lifetime references from a type node.
    fn extract_from_type(&mut self, type_node: Node, result: &mut LifetimeExtractionResult) {
        self.walk_type_for_lifetimes(type_node, result);
    }

    /// Recursively walk a type node looking for lifetime references.
    fn walk_type_for_lifetimes(&mut self, node: Node, result: &mut LifetimeExtractionResult) {
        match node.kind() {
            "reference_type" => {
                // &'a T pattern
                // Look for lifetime child (may not be a named field)
                let lifetime = node.child_by_field_name("lifetime").or_else(|| {
                    node.children(&mut node.walk())
                        .find(|c| c.kind() == "lifetime")
                });

                if let Some(lifetime) = lifetime {
                    let lifetime_name = self.node_text(lifetime);
                    let constraint_kind = if lifetime_name == "'static" {
                        LifetimeConstraintKind::Static
                    } else {
                        LifetimeConstraintKind::Reference
                    };

                    let target_qualified = if lifetime_name == "'static" {
                        "::static".to_string()
                    } else {
                        self.lifetime_map
                            .get(&lifetime_name)
                            .cloned()
                            .unwrap_or_else(|| {
                                format!("{}::{}", self.owner_qualified, lifetime_name)
                            })
                    };

                    result.edges.push(LifetimeEdge {
                        source_qualified: self.owner_qualified.clone(),
                        target_qualified,
                        constraint_kind,
                        span: Some(span_from_node(lifetime)),
                    });
                } else {
                    // Reference without explicit lifetime - this is an elided lifetime
                    // Record as limitation since elided lifetime resolution requires rust-analyzer
                    self.confidence.add_limitation(
                        "Elided lifetime in reference type; full resolution requires rust-analyzer",
                    );
                }
            }
            "dynamic_type" | "trait_object_type" => {
                // dyn Trait + 'a pattern
                self.extract_trait_object_lifetime(node, result);
            }
            "impl_type" | "opaque_type" => {
                // impl Trait + 'a pattern
                self.extract_impl_trait_lifetime(node, result);
            }
            "type_arguments" => {
                // Extract lifetimes from generic type arguments: SomeType<'a, 'b>
                self.extract_type_argument_lifetimes(node, result);
            }
            _ => {}
        }

        // Recurse into children
        for i in 0..node.child_count() {
            let child_index = u32::try_from(i)
                .unwrap_or_else(|_| unreachable!("tree-sitter child index exceeds u32"));
            if let Some(child) = node.child(child_index) {
                self.walk_type_for_lifetimes(child, result);
            }
        }
    }

    /// Extract lifetime from trait object (dyn Trait + 'a).
    fn extract_trait_object_lifetime(&self, node: Node, result: &mut LifetimeExtractionResult) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "lifetime" {
                let lifetime_name = self.node_text(child);
                let target_qualified = if lifetime_name == "'static" {
                    "::static".to_string()
                } else {
                    self.lifetime_map
                        .get(&lifetime_name)
                        .cloned()
                        .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, lifetime_name))
                };

                result.edges.push(LifetimeEdge {
                    source_qualified: self.owner_qualified.clone(),
                    target_qualified,
                    constraint_kind: LifetimeConstraintKind::TraitObject,
                    span: Some(span_from_node(child)),
                });
            }
        }
    }

    /// Extract lifetime from impl Trait (impl Trait + 'a).
    fn extract_impl_trait_lifetime(&self, node: Node, result: &mut LifetimeExtractionResult) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "lifetime" {
                let lifetime_name = self.node_text(child);
                let target_qualified = if lifetime_name == "'static" {
                    "::static".to_string()
                } else {
                    self.lifetime_map
                        .get(&lifetime_name)
                        .cloned()
                        .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, lifetime_name))
                };

                result.edges.push(LifetimeEdge {
                    source_qualified: self.owner_qualified.clone(),
                    target_qualified,
                    constraint_kind: LifetimeConstraintKind::ImplTrait,
                    span: Some(span_from_node(child)),
                });
            }
        }
    }

    /// Extract lifetimes from type arguments (`SomeType`<'a, 'b>).
    fn extract_type_argument_lifetimes(&self, node: Node, result: &mut LifetimeExtractionResult) {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "lifetime" {
                let lifetime_name = self.node_text(child);
                let constraint_kind = if lifetime_name == "'static" {
                    LifetimeConstraintKind::Static
                } else {
                    LifetimeConstraintKind::Reference
                };

                let target_qualified = if lifetime_name == "'static" {
                    "::static".to_string()
                } else {
                    self.lifetime_map
                        .get(&lifetime_name)
                        .cloned()
                        .unwrap_or_else(|| format!("{}::{}", self.owner_qualified, lifetime_name))
                };

                result.edges.push(LifetimeEdge {
                    source_qualified: self.owner_qualified.clone(),
                    target_qualified,
                    constraint_kind,
                    span: Some(span_from_node(child)),
                });
            }
        }
    }

    /// Get the text content of a node.
    fn node_text(&self, node: Node) -> String {
        node.utf8_text(self.content)
            .map(ToString::to_string)
            .unwrap_or_default()
    }
}

/// Create a span from a tree-sitter node.
fn span_from_node(node: Node) -> Span {
    let start = node.start_position();
    let end = node.end_position();
    Span::new(
        Position::new(start.row, start.column),
        Position::new(end.row, end.column),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confidence::ConfidenceLevel;
    use tree_sitter::Parser;

    fn parse_rust(code: &str) -> tree_sitter::Tree {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        parser.parse(code, None).unwrap()
    }

    fn extract_lifetimes(code: &str) -> LifetimeExtractionResult {
        let tree = parse_rust(code);
        let root = tree.root_node();
        let mut confidence = ConfidenceTracker::default();

        // Find the first function or struct
        let mut cursor = root.walk();
        for child in root.children(&mut cursor) {
            match child.kind() {
                "function_item" | "struct_item" | "impl_item" => {
                    let mut extractor = LifetimeExtractor::new(
                        code.as_bytes(),
                        "test_owner".to_string(),
                        &mut confidence,
                    );
                    return extractor.extract(child);
                }
                _ => {}
            }
        }

        LifetimeExtractionResult::default()
    }

    #[test]
    fn test_extract_lifetime_parameters() {
        let result = extract_lifetimes("fn foo<'a, 'b>(x: &'a str, y: &'b str) {}");

        // Should have 2 lifetime nodes
        assert_eq!(result.nodes.len(), 2);
        assert!(result.nodes.iter().any(|n| n.name == "'a"));
        assert!(result.nodes.iter().any(|n| n.name == "'b"));
    }

    #[test]
    fn test_extract_outlives_constraint() {
        let result = extract_lifetimes("fn foo<'a, 'b>(x: &'a str) where 'a: 'b {}");

        // Should have outlives edge
        let outlives_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Outlives)
            .collect();
        assert!(
            !outlives_edges.is_empty(),
            "No Outlives edges found. All edges: {:?}",
            result.edges
        );
    }

    #[test]
    fn test_extract_type_bound() {
        let result = extract_lifetimes("fn foo<'a, T: 'a>(x: T) {}");

        // Should have type bound edge
        let type_bound_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::TypeBound)
            .collect();
        assert!(!type_bound_edges.is_empty());
    }

    #[test]
    fn test_extract_reference_lifetime() {
        let result = extract_lifetimes("fn foo<'a>(x: &'a str) {}");

        // Should have reference edge
        let reference_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Reference)
            .collect();
        assert!(!reference_edges.is_empty());
    }

    #[test]
    fn test_extract_static_lifetime() {
        let result = extract_lifetimes("fn foo(x: &'static str) {}");

        // Should have static edge
        let static_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Static)
            .collect();
        assert!(!static_edges.is_empty());
        assert!(
            static_edges
                .iter()
                .any(|e| e.target_qualified == "::static")
        );
    }

    #[test]
    fn test_empty_function() {
        let result = extract_lifetimes("fn foo() {}");
        assert!(result.nodes.is_empty());
        assert!(result.edges.is_empty());
    }

    #[test]
    fn test_lifetime_extraction_result_merge() {
        let mut result1 = LifetimeExtractionResult::default();
        result1.nodes.push(LifetimeNode {
            name: "'a".to_string(),
            qualified_name: "foo::'a".to_string(),
            span: None,
        });

        let mut result2 = LifetimeExtractionResult::default();
        result2.nodes.push(LifetimeNode {
            name: "'b".to_string(),
            qualified_name: "bar::'b".to_string(),
            span: None,
        });

        result1.merge(result2);
        assert_eq!(result1.nodes.len(), 2);
    }

    #[test]
    fn test_extract_impl_trait_path_lifetime() {
        // Test: impl<'a> Trait<'a> for Foo<'a>
        // Should extract lifetime references from trait path
        let source = r#"
trait MyTrait<'a> {
    fn get(&self) -> &'a str;
}

struct Foo<'a> {
    data: &'a str,
}

impl<'a> MyTrait<'a> for Foo<'a> {
    fn get(&self) -> &'a str {
        self.data
    }
}
"#;
        let tree = parse_rust(source);
        let root = tree.root_node();

        // Find the impl_item
        let mut impl_node = None;
        for child in root.children(&mut root.walk()) {
            if child.kind() == "impl_item" {
                impl_node = Some(child);
                break;
            }
        }

        let impl_node = impl_node.expect("impl_item not found");
        let mut confidence = ConfidenceTracker::new(ConfidenceLevel::Partial);
        let mut extractor =
            LifetimeExtractor::new(source.as_bytes(), "Foo".to_string(), &mut confidence);
        let result = extractor.extract(impl_node);

        // Should have lifetime node for 'a
        assert!(!result.nodes.is_empty(), "Should extract lifetime nodes");
        assert!(
            result.nodes.iter().any(|n| n.name == "'a"),
            "Should find lifetime 'a"
        );

        // Should have Reference edges for trait path and impl type lifetimes
        let reference_edges: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.constraint_kind == LifetimeConstraintKind::Reference)
            .collect();
        // At least 2 references: one from MyTrait<'a> and one from Foo<'a>
        assert!(
            reference_edges.len() >= 2,
            "Should have at least 2 Reference edges (trait path and impl type), found: {:?}",
            reference_edges
        );
    }
}
