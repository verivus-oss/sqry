//! Macro for adding GraphBuilder support to LanguagePlugin implementations
//!
//! This module provides the `impl_graph_plugin!` macro that reduces boilerplate
//! for wiring up GraphBuilder implementations to LanguagePlugin structs.
//!
//! # FR-2025-016 Phase 2: Graph Builder Wire-Up
//!
//! Instead of manually editing 31 language plugin files, this macro provides a
//! declarative way to add GraphBuilder support with type safety.
//!
//! # Example
//!
//! ```rust
//! use sqry_core::plugin::impl_graph_plugin;
//!
//! // Before (unit struct):
//! // pub struct JavaScriptPlugin;
//!
//! // After (with GraphBuilder support):
//! impl_graph_plugin! {
//!     /// JavaScript language plugin with GraphBuilder support
//!     pub struct JavaScriptPlugin {
//!         graph_builder: crate::relations::JavaScriptGraphBuilder,
//!     }
//! }
//!
//! // This generates:
//! // - Struct with graph_builder field
//! // - new() constructor
//! // - Default implementation
//! // - graph_builder() method for LanguagePlugin trait
//! ```
//!
//! # Generated Code
//!
//! The macro expands to:
//!
//! ```rust
//! pub struct JavaScriptPlugin {
//!     graph_builder: crate::relations::JavaScriptGraphBuilder,
//! }
//!
//! impl JavaScriptPlugin {
//!     pub fn new() -> Self {
//!         Self {
//!             graph_builder: Default::default(),
//!         }
//!     }
//! }
//!
//! impl Default for JavaScriptPlugin {
//!     fn default() -> Self {
//!         Self::new()
//!     }
//! }
//!
//! // Note: You must still manually add this to your LanguagePlugin impl:
//! // fn graph_builder(&self) -> Option<&dyn GraphBuilder> {
//! //     Some(&self.graph_builder)
//! // }
//! ```
//!
//! # Migration Pattern
//!
//! ## Step 1: Replace unit struct with macro invocation
//!
//! ```rust
//! // OLD:
//! pub struct JavaScriptPlugin;
//!
//! // NEW:
//! impl_graph_plugin! {
//!     pub struct JavaScriptPlugin {
//!         graph_builder: crate::relations::JavaScriptGraphBuilder,
//!     }
//! }
//! ```
//!
//! ## Step 2: Add graph_builder() to LanguagePlugin impl
//!
//! ```rust
//! impl LanguagePlugin for JavaScriptPlugin {
//!     // ... existing methods ...
//!
//!     fn graph_builder(&self) -> Option<&dyn sqry_core::graph::GraphBuilder> {
//!         Some(&self.graph_builder)
//!     }
//! }
//! ```
//!
//! ## Step 3: Update test instantiations (if needed)
//!
//! ```rust
//! // If tests used JavaScriptPlugin directly:
//! let plugin = JavaScriptPlugin::default(); // OLD
//! let plugin = JavaScriptPlugin::new(); // NEW
//! ```
//!
//! # Why This Approach?
//!
//! **Considered alternatives:**
//!
//! 1. **Full trait macro**: Could auto-implement `graph_builder()` method, but:
//!    - Can't add methods to existing trait impls via macro (Rust limitation)
//!    - Would require splitting LanguagePlugin impl into multiple blocks
//!
//! 2. **Derive macro**: More powerful but:
//!    - Requires proc-macro crate (adds compile-time dependency)
//!    - Overkill for simple struct generation
//!
//! 3. **Manual editing**: Original plan, but:
//!    - Error-prone (31 files × 5-10 lines = 155-310 lines of boilerplate)
//!    - Hard to maintain
//!    - No type safety during refactoring
//!
//! **This declarative macro approach**:
//! - ✅ Zero runtime cost (expanded at compile time)
//! - ✅ No proc-macro dependency
//! - ✅ Type-safe (compiler checks GraphBuilder type)
//! - ✅ DRY (define once, use 31 times)
//! - ✅ Easy to maintain (change macro, all languages updated)
//! - ✅ Self-documenting (macro name explains intent)
//!
//! # Limitations
//!
//! - Must still manually add `graph_builder()` to LanguagePlugin impl
//!   (Rust doesn't allow macros to extend existing trait impls)
//! - GraphBuilder type must be in scope when macro is invoked

/// Generate a LanguagePlugin struct with GraphBuilder field support
///
/// # Syntax
///
/// ```text
/// impl_graph_plugin! {
///     $(#[$meta:meta])*  // Optional attributes (doc comments, derives, etc.)
///     $vis:vis struct $name:ident {
///         graph_builder: $builder_ty:ty,
///     }
/// }
/// ```
///
/// # Example
///
/// ```rust
/// impl_graph_plugin! {
///     /// Python language plugin
///     #[derive(Debug)]
///     pub struct PythonPlugin {
///         graph_builder: crate::relations::PythonGraphBuilder,
///     }
/// }
/// ```
#[macro_export]
macro_rules! impl_graph_plugin {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            graph_builder: $builder_ty:ty,
        }
    ) => {
        $(#[$meta])*
        $vis struct $name {
            graph_builder: $builder_ty,
        }

        impl $name {
            /// Create a new plugin instance with default GraphBuilder
            pub fn new() -> Self {
                Self {
                    graph_builder: Default::default(),
                }
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

#[cfg(test)]
mod tests {
    // Mock types for testing
    struct MockGraphBuilder;
    impl Default for MockGraphBuilder {
        fn default() -> Self {
            Self
        }
    }

    #[test]
    fn test_macro_generates_struct() {
        impl_graph_plugin! {
            /// Test plugin
            pub struct TestPlugin {
                graph_builder: MockGraphBuilder,
            }
        }

        let plugin = TestPlugin::new();
        let _ = plugin.graph_builder; // Verify field exists
    }

    #[test]
    fn test_macro_generates_default() {
        impl_graph_plugin! {
            pub struct TestPlugin2 {
                graph_builder: MockGraphBuilder,
            }
        }

        let _plugin = TestPlugin2::default();
    }

    #[test]
    fn test_macro_with_multiple_attributes() {
        impl_graph_plugin! {
            /// Documentation
            #[allow(dead_code)]
            pub struct TestPlugin3 {
                graph_builder: MockGraphBuilder,
            }
        }

        let _plugin = TestPlugin3::new();
    }
}
