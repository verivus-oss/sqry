//! Trait method binding resolution for Rust.
//!
//! This module resolves trait method calls to their concrete implementations,
//! creating `TraitMethodBinding` edges in the graph.
//!
//! # Resolution Strategy
//!
//! ## AST-Only Mode
//!
//! 1. Explicit type annotation in let binding: `let x: Vec<i32> = ...; x.iter()`
//! 2. Constructor call chain: `Vec::new().iter()`
//! 3. self receiver in impl block: `impl Foo { fn bar(&self) { self.method() } }`
//!
//! ## With rust-analyzer
//!
//! Type inference via `ra_ide` HIR provides complete binding resolution.
//!
//! # Query Semantics
//!
//! `TraitMethodBinding` edges are **NOT** included in `callers` by default.
//! Use `--include-trait-bindings` flag or dedicated query.

use crate::confidence::ConfidenceTracker;
use std::collections::HashMap;

/// Information about a resolved impl method.
#[derive(Debug, Clone)]
pub struct ImplMethodInfo {
    /// The trait providing the method
    pub trait_name: String,
    /// The implementing type
    pub impl_type: String,
    /// Qualified name of the impl method
    pub method_qualified: String,
}

/// Result of trait method binding resolution.
#[derive(Debug)]
pub enum BindingResult {
    /// Single unambiguous binding
    Single(ImplMethodInfo),
    /// Multiple possible bindings (all emitted with `is_ambiguous=true`)
    Multiple(Vec<ImplMethodInfo>),
    /// No binding found (no edge emitted)
    NotFound { reason: String },
    /// Receiver type cannot be inferred in AST-only mode
    UnknownReceiverType { reason: String },
    /// Deferred to rust-analyzer
    DeferToRa,
}

/// Trait method binder for resolving method calls to implementations.
pub struct TraitMethodBinder {
    /// Map of type -> trait -> method impls
    /// Format: `impl_type` -> `trait_name` -> Vec<`method_qualified`>
    trait_impls: HashMap<String, HashMap<String, Vec<String>>>,
    /// Map of variable -> inferred type (from let annotations)
    local_types: HashMap<String, String>,
    /// The current impl type (when inside an impl block)
    current_impl_type: Option<String>,
}

impl Default for TraitMethodBinder {
    fn default() -> Self {
        Self::new()
    }
}

impl TraitMethodBinder {
    /// Create a new trait method binder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            trait_impls: HashMap::new(),
            local_types: HashMap::new(),
            current_impl_type: None,
        }
    }

    /// Register a trait implementation.
    ///
    /// Call this when encountering `impl Trait for Type` blocks.
    pub fn register_impl(&mut self, impl_type: &str, trait_name: &str, method_qualified: &str) {
        self.trait_impls
            .entry(impl_type.to_string())
            .or_default()
            .entry(trait_name.to_string())
            .or_default()
            .push(method_qualified.to_string());
    }

    /// Register a local variable's type from a let binding.
    ///
    /// Example: `let x: Vec<i32> = ...;` -> register `x` -> `Vec<i32>`
    pub fn register_local_type(&mut self, var_name: &str, type_name: &str) {
        self.local_types
            .insert(var_name.to_string(), type_name.to_string());
    }

    /// Set the current impl type (when entering an impl block).
    pub fn set_current_impl_type(&mut self, impl_type: Option<String>) {
        self.current_impl_type = impl_type;
    }

    /// Get the current impl type.
    #[must_use]
    pub fn current_impl_type(&self) -> Option<&str> {
        self.current_impl_type.as_deref()
    }

    /// Clear local types (when exiting a function scope).
    pub fn clear_local_types(&mut self) {
        self.local_types.clear();
    }

    /// Infer receiver type using AST-only heuristics.
    ///
    /// Returns the type name if it can be inferred, or `None` if unknown.
    #[must_use]
    pub fn infer_receiver_type(&self, receiver_text: &str) -> Option<String> {
        // Strategy 1: Check local variable types
        if let Some(type_name) = self.local_types.get(receiver_text) {
            // Strip reference and mutability markers from the type
            return Some(Self::normalize_type(type_name));
        }

        // Strategy 2: self receiver in impl block
        if receiver_text == "self" || receiver_text == "&self" || receiver_text == "&mut self" {
            return self.current_impl_type.clone();
        }

        // Strategy 3: Constructor pattern (Type::new())
        // This would require more context from the call expression

        None
    }

    /// Normalize a type by stripping references and mutability markers.
    ///
    /// Examples:
    /// - `&MyStruct` -> `MyStruct`
    /// - `&mut MyStruct` -> `MyStruct`
    /// - `MyStruct` -> `MyStruct`
    #[must_use]
    fn normalize_type(type_name: &str) -> String {
        type_name
            .trim()
            .trim_start_matches("&mut ")
            .trim_start_matches('&')
            .to_string()
    }

    /// Resolve a method call to its binding.
    ///
    /// # Arguments
    ///
    /// * `receiver_type` - The inferred receiver type (or None if unknown)
    /// * `method_name` - The method being called
    /// * `ra_available` - Whether rust-analyzer is available for fallback
    /// * `confidence` - Confidence tracker for recording limitations
    pub fn resolve_method_call(
        &self,
        receiver_type: Option<&str>,
        method_name: &str,
        ra_available: bool,
        confidence: &mut ConfidenceTracker,
    ) -> BindingResult {
        // Handle unknown receiver type
        let receiver_type = match receiver_type {
            Some(t) => t,
            None if ra_available => return BindingResult::DeferToRa,
            None => {
                return BindingResult::UnknownReceiverType {
                    reason: "Cannot infer receiver type in AST-only mode".to_string(),
                };
            }
        };

        // Find impls for this type
        let Some(trait_impls) = self.trait_impls.get(receiver_type) else {
            return BindingResult::NotFound {
                reason: format!("No trait impls found for type '{receiver_type}'"),
            };
        };

        // Collect all methods matching this name across all traits
        let mut matches = Vec::new();
        for (trait_name, methods) in trait_impls {
            for method_qualified in methods {
                if method_qualified.ends_with(&format!("::{method_name}")) {
                    matches.push(ImplMethodInfo {
                        trait_name: trait_name.clone(),
                        impl_type: receiver_type.to_string(),
                        method_qualified: method_qualified.clone(),
                    });
                }
            }
        }

        match matches.len() {
            0 => BindingResult::NotFound {
                reason: format!(
                    "No method '{method_name}' found in trait impls for '{receiver_type}'"
                ),
            },
            1 => BindingResult::Single(matches.remove(0)),
            _ => {
                confidence.add_limitation(&format!(
                    "Multiple trait impls match for '{receiver_type}::{method_name}'"
                ));
                BindingResult::Multiple(matches)
            }
        }
    }

    /// Resolve a method call given call expression context.
    ///
    /// This is the high-level entry point that combines receiver inference
    /// and method resolution.
    pub fn resolve_call(
        &self,
        receiver_text: &str,
        method_name: &str,
        ra_available: bool,
        confidence: &mut ConfidenceTracker,
    ) -> BindingResult {
        let receiver_type = self.infer_receiver_type(receiver_text);
        self.resolve_method_call(
            receiver_type.as_deref(),
            method_name,
            ra_available,
            confidence,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_impl() {
        let mut binder = TraitMethodBinder::new();
        binder.register_impl("Vec<T>", "Iterator", "Vec<T>::iter");
        binder.register_impl("Vec<T>", "IntoIterator", "Vec<T>::into_iter");

        assert!(binder.trait_impls.contains_key("Vec<T>"));
        assert!(binder.trait_impls["Vec<T>"].contains_key("Iterator"));
    }

    #[test]
    fn test_register_local_type() {
        let mut binder = TraitMethodBinder::new();
        binder.register_local_type("x", "Vec<i32>");

        assert_eq!(
            binder.infer_receiver_type("x"),
            Some("Vec<i32>".to_string())
        );
        assert_eq!(binder.infer_receiver_type("y"), None);
    }

    #[test]
    fn test_infer_self_receiver() {
        let mut binder = TraitMethodBinder::new();
        binder.set_current_impl_type(Some("MyStruct".to_string()));

        assert_eq!(
            binder.infer_receiver_type("self"),
            Some("MyStruct".to_string())
        );
        assert_eq!(
            binder.infer_receiver_type("&self"),
            Some("MyStruct".to_string())
        );
    }

    #[test]
    fn test_resolve_single_binding() {
        let mut binder = TraitMethodBinder::new();
        binder.register_impl("Vec<T>", "Iterator", "Vec<T>::iter");
        binder.register_local_type("v", "Vec<T>");

        let mut confidence = ConfidenceTracker::default();
        let result = binder.resolve_call("v", "iter", false, &mut confidence);

        match result {
            BindingResult::Single(info) => {
                assert_eq!(info.trait_name, "Iterator");
                assert_eq!(info.impl_type, "Vec<T>");
                assert_eq!(info.method_qualified, "Vec<T>::iter");
            }
            _ => panic!("Expected Single binding"),
        }
    }

    #[test]
    fn test_resolve_multiple_bindings() {
        let mut binder = TraitMethodBinder::new();
        binder.register_impl("MyType", "TraitA", "MyType::method");
        binder.register_impl("MyType", "TraitB", "MyType::method");
        binder.register_local_type("x", "MyType");

        let mut confidence = ConfidenceTracker::default();
        let result = binder.resolve_call("x", "method", false, &mut confidence);

        match result {
            BindingResult::Multiple(infos) => {
                assert_eq!(infos.len(), 2);
                assert!(
                    confidence.has_limitation("Multiple trait impls match for 'MyType::method'")
                );
            }
            _ => panic!("Expected Multiple bindings"),
        }
    }

    #[test]
    fn test_resolve_unknown_receiver() {
        let binder = TraitMethodBinder::new();
        let mut confidence = ConfidenceTracker::default();

        let result = binder.resolve_call("unknown_var", "method", false, &mut confidence);

        match result {
            BindingResult::UnknownReceiverType { reason } => {
                assert!(reason.contains("Cannot infer"));
            }
            _ => panic!("Expected UnknownReceiverType"),
        }
    }

    #[test]
    fn test_resolve_defer_to_ra() {
        let binder = TraitMethodBinder::new();
        let mut confidence = ConfidenceTracker::default();

        let result = binder.resolve_call("unknown_var", "method", true, &mut confidence);

        assert!(matches!(result, BindingResult::DeferToRa));
    }

    #[test]
    fn test_resolve_not_found() {
        let mut binder = TraitMethodBinder::new();
        binder.register_local_type("x", "MyType");

        let mut confidence = ConfidenceTracker::default();
        let result = binder.resolve_call("x", "method", false, &mut confidence);

        match result {
            BindingResult::NotFound { reason } => {
                assert!(reason.contains("No trait impls found"));
            }
            _ => panic!("Expected NotFound"),
        }
    }

    #[test]
    fn test_clear_local_types() {
        let mut binder = TraitMethodBinder::new();
        binder.register_local_type("x", "Vec<i32>");

        assert!(binder.infer_receiver_type("x").is_some());

        binder.clear_local_types();

        assert!(binder.infer_receiver_type("x").is_none());
    }

    #[test]
    fn test_normalize_type() {
        assert_eq!(TraitMethodBinder::normalize_type("MyStruct"), "MyStruct");
        assert_eq!(TraitMethodBinder::normalize_type("&MyStruct"), "MyStruct");
        assert_eq!(
            TraitMethodBinder::normalize_type("&mut MyStruct"),
            "MyStruct"
        );
        assert_eq!(
            TraitMethodBinder::normalize_type("  &MyStruct  "),
            "MyStruct"
        );
    }

    #[test]
    fn test_resolve_with_reference_type() {
        let mut binder = TraitMethodBinder::new();
        binder.register_impl("MyStruct", "MyTrait", "MyStruct::method");
        binder.register_local_type("x", "&MyStruct"); // Note: with reference

        let mut confidence = ConfidenceTracker::default();
        let result = binder.resolve_call("x", "method", false, &mut confidence);

        match result {
            BindingResult::Single(info) => {
                assert_eq!(info.trait_name, "MyTrait");
                assert_eq!(info.impl_type, "MyStruct");
                assert_eq!(info.method_qualified, "MyStruct::method");
            }
            _ => panic!("Expected Single binding for reference type"),
        }
    }

    #[test]
    fn test_resolve_with_mut_reference_type() {
        let mut binder = TraitMethodBinder::new();
        binder.register_impl("Vec<T>", "Iterator", "Vec<T>::iter");
        binder.register_local_type("v", "&mut Vec<T>"); // Note: mutable reference

        let mut confidence = ConfidenceTracker::default();
        let result = binder.resolve_call("v", "iter", false, &mut confidence);

        match result {
            BindingResult::Single(info) => {
                assert_eq!(info.trait_name, "Iterator");
                assert_eq!(info.impl_type, "Vec<T>");
            }
            _ => panic!("Expected Single binding for mutable reference type"),
        }
    }
}
