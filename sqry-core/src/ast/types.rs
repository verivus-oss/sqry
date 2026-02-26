//! Core types for AST context representation
//!
//! This module defines the types used to represent AST context:
//! - `ContextKind`: Types of AST nodes (function, class, etc.)
//! - `ContextItem`: A single context node with location info
//! - Context: Full context chain (parent, ancestors, immediate)
//! - `ContextualMatch`: Match metadata with AST context

use crate::graph::unified::node::NodeKind;
use std::path::PathBuf;

/// Kind of AST context node
///
/// Represents the type of code construct in the AST hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextKind {
    /// Function definition
    Function,
    /// Method definition (function inside class/struct/impl)
    Method,
    /// Class definition
    Class,
    /// Struct definition
    Struct,
    /// Interface/protocol definition
    Interface,
    /// Enum definition
    Enum,
    /// Trait definition
    Trait,
    /// Module/namespace
    Module,
    /// Constant definition
    Constant,
    /// Variable definition
    Variable,
    /// Type alias
    TypeAlias,
    /// Implementation block (Rust)
    Impl,
}

impl ContextKind {
    /// Convert from `NodeKind` to `ContextKind`
    #[must_use]
    pub fn from_node_kind(node_kind: NodeKind) -> Option<Self> {
        let kind = match node_kind {
            NodeKind::Function => ContextKind::Function,
            NodeKind::Method => ContextKind::Method,
            NodeKind::Class => ContextKind::Class,
            NodeKind::Struct => ContextKind::Struct,
            NodeKind::Interface => ContextKind::Interface,
            NodeKind::Enum => ContextKind::Enum,
            NodeKind::Trait => ContextKind::Trait,
            NodeKind::Module
            | NodeKind::Import
            | NodeKind::Component
            | NodeKind::StyleRule
            | NodeKind::StyleAtRule => ContextKind::Module,
            NodeKind::Constant => ContextKind::Constant,
            NodeKind::Variable
            | NodeKind::Property
            | NodeKind::Parameter
            | NodeKind::StyleVariable => ContextKind::Variable,
            NodeKind::Type => ContextKind::TypeAlias,
            _ => return None,
        };
        Some(kind)
    }
}

/// A single item in the AST context chain
///
/// Represents one node in the ancestor chain, with its name and location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    /// Name of the context node
    pub name: String,
    /// Kind of the context node
    pub kind: ContextKind,
    /// Starting line (1-based)
    pub start_line: u32,
    /// Ending line (1-based)
    pub end_line: u32,
    /// Starting byte offset
    pub start_byte: usize,
    /// Ending byte offset
    pub end_byte: usize,
}

impl ContextItem {
    /// Create a new context item
    #[must_use]
    pub fn new(
        name: String,
        kind: ContextKind,
        start_line: u32,
        end_line: u32,
        start_byte: usize,
        end_byte: usize,
    ) -> Self {
        Self {
            name,
            kind,
            start_line,
            end_line,
            start_byte,
            end_byte,
        }
    }
}

/// AST context for a symbol
///
/// Contains the full context chain from the symbol up to the root:
/// - immediate: The symbol itself
/// - parent: Direct parent (if any)
/// - ancestors: All ancestors from parent to root (inner to outer)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    /// The symbol/node itself
    pub immediate: ContextItem,
    /// Direct parent node (None for top-level)
    pub parent: Option<ContextItem>,
    /// Ancestor chain from parent to root (inner to outer)
    pub ancestors: Vec<ContextItem>,
    /// Kind of the immediate node
    pub kind: ContextKind,
}

impl Context {
    /// Create a new context
    #[must_use]
    pub fn new(
        immediate: ContextItem,
        parent: Option<ContextItem>,
        ancestors: Vec<ContextItem>,
    ) -> Self {
        let kind = immediate.kind;
        Self {
            immediate,
            parent,
            ancestors,
            kind,
        }
    }

    /// Get the nesting depth (1 = top-level)
    ///
    /// Depth represents "how many levels deep from root":
    /// - Top-level (no parent): depth 1
    /// - One level nested: depth 2
    /// - Two levels nested: depth 3
    ///
    /// Formula: 1 (self) + `ancestors.len()` + parent (if exists)
    #[must_use]
    pub fn depth(&self) -> usize {
        let parent_depth = usize::from(self.parent.is_some());
        1 + self.ancestors.len() + parent_depth
    }

    /// Build the full symbol path (e.g., "`Module::Class::method`")
    ///
    /// Path is built from ancestors (outermost first) to immediate.
    #[must_use]
    pub fn path(&self) -> String {
        let mut parts = Vec::new();

        // Add ancestors (outermost to innermost)
        for ancestor in self.ancestors.iter().rev() {
            parts.push(ancestor.name.as_str());
        }

        // Add parent
        if let Some(ref parent) = self.parent {
            parts.push(parent.name.as_str());
        }

        // Add immediate
        parts.push(self.immediate.name.as_str());

        parts.join("::")
    }
}

/// A match with its full AST context
///
/// This is the primary type returned by context extraction, combining
/// node metadata with its surrounding context information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextualMatch {
    /// Node name
    pub name: String,
    /// File path containing the symbol
    pub file_path: PathBuf,
    /// Starting line (1-based)
    pub start_line: u32,
    /// Starting column (0-based)
    pub start_column: u32,
    /// Ending line (1-based)
    pub end_line: u32,
    /// Ending column (0-based)
    pub end_column: u32,
    /// AST context (parent, ancestors)
    pub context: Context,
    /// Source language
    pub language: String,
}

/// Location data for a contextual match
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextualMatchLocation {
    /// File path containing the symbol
    pub file_path: PathBuf,
    /// Starting line (1-based)
    pub start_line: u32,
    /// Starting column (0-based)
    pub start_column: u32,
    /// Ending line (1-based)
    pub end_line: u32,
    /// Ending column (0-based)
    pub end_column: u32,
}

impl ContextualMatchLocation {
    /// Create a new contextual match location.
    #[must_use]
    pub fn new(
        file_path: PathBuf,
        start_line: u32,
        start_column: u32,
        end_line: u32,
        end_column: u32,
    ) -> Self {
        Self {
            file_path,
            start_line,
            start_column,
            end_line,
            end_column,
        }
    }
}

impl ContextualMatch {
    /// Create a new contextual match
    #[must_use]
    pub fn new(
        name: String,
        location: ContextualMatchLocation,
        context: Context,
        language: String,
    ) -> Self {
        Self {
            name,
            file_path: location.file_path,
            start_line: location.start_line,
            start_column: location.start_column,
            end_line: location.end_line,
            end_column: location.end_column,
            context,
            language,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_kind_from_symbol_type() {
        assert_eq!(
            ContextKind::from_node_kind(NodeKind::Function),
            Some(ContextKind::Function)
        );
        assert_eq!(
            ContextKind::from_node_kind(NodeKind::Class),
            Some(ContextKind::Class)
        );
        assert_eq!(
            ContextKind::from_node_kind(NodeKind::Method),
            Some(ContextKind::Method)
        );
        assert_eq!(
            ContextKind::from_node_kind(NodeKind::Struct),
            Some(ContextKind::Struct)
        );
        assert_eq!(ContextKind::from_node_kind(NodeKind::CallSite), None);
    }

    #[test]
    fn test_context_depth() {
        // Top-level (no parent) - NOW depth 1, not 0
        let immediate = ContextItem::new("foo".to_string(), ContextKind::Function, 1, 5, 0, 50);
        let ctx = Context::new(immediate, None, vec![]);
        assert_eq!(ctx.depth(), 1); // FIXED: top-level is depth 1

        // One level deep (has parent, no ancestors) - NOW depth 2
        let immediate = ContextItem::new("bar".to_string(), ContextKind::Method, 10, 15, 100, 200);
        let parent = ContextItem::new("MyClass".to_string(), ContextKind::Class, 5, 20, 50, 300);
        let ctx = Context::new(immediate, Some(parent), vec![]);
        assert_eq!(ctx.depth(), 2); // FIXED: one level nested is depth 2

        // Two levels deep (has parent and 1 ancestor) - NOW depth 3
        let immediate = ContextItem::new("baz".to_string(), ContextKind::Method, 12, 14, 150, 180);
        let parent = ContextItem::new(
            "InnerClass".to_string(),
            ContextKind::Class,
            10,
            15,
            120,
            200,
        );
        let ancestor =
            ContextItem::new("OuterClass".to_string(), ContextKind::Class, 5, 20, 50, 300);
        let ctx = Context::new(immediate, Some(parent), vec![ancestor]);
        assert_eq!(ctx.depth(), 3); // FIXED: two levels nested is depth 3
    }

    #[test]
    fn test_context_depth_deeply_nested() {
        // Test deep nesting (3 ancestors + parent)
        let immediate = ContextItem::new(
            "deeply_nested".to_string(),
            ContextKind::Method,
            20,
            22,
            300,
            320,
        );
        let parent = ContextItem::new("Level3".to_string(), ContextKind::Class, 18, 23, 280, 350);
        let ancestors = vec![
            ContextItem::new("Level0".to_string(), ContextKind::Module, 1, 30, 0, 500),
            ContextItem::new("Level1".to_string(), ContextKind::Class, 5, 25, 50, 400),
            ContextItem::new("Level2".to_string(), ContextKind::Class, 10, 24, 150, 380),
        ];
        let ctx = Context::new(immediate, Some(parent), ancestors);

        // Formula: 1 (self) + 3 (ancestors) + 1 (parent) = 5
        assert_eq!(ctx.depth(), 5, "Deeply nested should be depth 5");
    }

    #[test]
    fn test_context_path() {
        // Path with module, class, and method
        let immediate =
            ContextItem::new("process".to_string(), ContextKind::Method, 15, 20, 200, 300);
        let parent = ContextItem::new("Handler".to_string(), ContextKind::Class, 10, 25, 150, 400);
        let ancestor = ContextItem::new("utils".to_string(), ContextKind::Module, 1, 30, 0, 500);
        let ctx = Context::new(immediate, Some(parent), vec![ancestor]);

        assert_eq!(ctx.path(), "utils::Handler::process");
    }

    #[test]
    fn test_context_path_top_level() {
        // Top-level function (no parent/ancestors)
        let immediate = ContextItem::new("main".to_string(), ContextKind::Function, 1, 10, 0, 100);
        let ctx = Context::new(immediate, None, vec![]);

        assert_eq!(ctx.path(), "main");
    }

    #[test]
    fn test_context_item_creation() {
        let item = ContextItem::new(
            "test_func".to_string(),
            ContextKind::Function,
            10,
            20,
            100,
            200,
        );

        assert_eq!(item.name, "test_func");
        assert_eq!(item.kind, ContextKind::Function);
        assert_eq!(item.start_line, 10);
        assert_eq!(item.end_line, 20);
        assert_eq!(item.start_byte, 100);
        assert_eq!(item.end_byte, 200);
    }
}
