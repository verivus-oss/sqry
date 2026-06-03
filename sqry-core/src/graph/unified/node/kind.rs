//! `NodeKind` enumeration for the unified graph architecture.
//!
//! This module defines `NodeKind`, which categorizes all code entities
//! that can be represented as nodes in the graph.
//!
//! # Design (, Appendix A1)
//!
//! The enumeration covers:
//! - **Core symbols**: Functions, methods, classes, interfaces, traits
//! - **Declarations**: Variables, constants, types, modules
//! - **Structural**: Call sites, components, services
//! - **Domain-specific**: Resources, endpoints, handlers

use std::fmt;

use serde::{Deserialize, Serialize};

/// Enumeration of code entity types that can be represented as graph nodes.
///
/// Each variant represents a distinct category of code symbol. The categorization
/// is language-agnostic to support cross-language analysis.
///
/// # Serialization
///
/// All variants serialize to their lowercase name for JSON interoperability:
/// - `Function` → `"function"`
/// - `CallSite` → `"call_site"`
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    // ==================== Core Symbols ====================
    /// A standalone function (not a method).
    Function,

    /// A method belonging to a class, struct, or trait.
    Method,

    /// A class definition (OOP languages).
    Class,

    /// An interface definition (TypeScript, Go, Java, etc.).
    Interface,

    /// A trait definition (Rust, Scala, etc.).
    Trait,

    /// A module or namespace declaration.
    Module,

    // ==================== Declarations ====================
    /// A variable binding (let, var, const in some languages).
    Variable,

    /// A constant value (const, static const, final).
    Constant,

    /// A type alias or typedef.
    Type,

    /// A struct definition.
    Struct,

    /// An enum definition.
    Enum,

    /// An enum variant.
    EnumVariant,

    /// A macro definition.
    Macro,

    /// A function parameter.
    Parameter,

    /// A class property or struct field.
    Property,

    // ==================== Structural ====================
    /// A call site (location where a call occurs).
    ///
    /// Call sites are used for fine-grained call graph analysis,
    /// allowing multiple edges from the same function.
    CallSite,

    /// An import statement or declaration.
    Import,

    /// An export statement or re-export.
    Export,

    // ==================== Styles (CSS/SCSS/Less) ====================
    /// A style rule (selector block).
    StyleRule,

    /// A style at-rule (@media, @keyframes, etc.).
    StyleAtRule,

    /// A style variable ($var or --var).
    StyleVariable,

    // ==================== Rust-Specific ====================
    /// A lifetime parameter pseudo-symbol.
    ///
    /// Lifetime nodes enable `LifetimeConstraint` edges to have proper
    /// source/target node IDs in the graph. Each lifetime parameter
    /// (e.g., `'a`, `'b`, `'static`) in a function/struct signature
    /// becomes a node.
    ///
    /// Qualified name format: `{owner}::'a` (e.g., `foo::'a` for `fn foo<'a>`)
    /// Special case: `'static` is represented as `::static`
    Lifetime,

    // ==================== Domain-Specific ====================
    /// A component (React, Vue, Angular, etc.).
    Component,

    /// A service class or service function.
    Service,

    /// A resource (REST, GraphQL, etc.).
    Resource,

    /// An API endpoint handler.
    Endpoint,

    /// A test function or test case.
    Test,

    // ==================== JVM Classpath (Track C) ====================
    /// Type parameter declaration (e.g., `T` in `class Foo<T>`).
    TypeParameter,

    /// Annotation type or usage (e.g., `@Override`, `@RequestMapping`).
    Annotation,

    /// Annotation element value (e.g., `value = "/api"` in `@RequestMapping`).
    AnnotationValue,

    /// Lambda or method reference target (e.g., `String::toUpperCase`).
    LambdaTarget,

    /// Java 9+ module declaration (from `module-info.java`).
    JavaModule,

    /// Enum constant (e.g., `MONDAY` in `enum DayOfWeek`).
    EnumConstant,

    // ==================== Go Concurrency (T2.4) ====================
    /// A statically-resolvable channel symbol (Go).
    ///
    /// Represents the alias-class of channel values traced by the
    /// sqry-lang-go Phase 1 alias resolver (see
    /// `docs/development/go-channels-and-generic-instantiation/02_DESIGN.md` §4.1).
    /// Each `Channel` node is the *target* of one or more `ChannelPeer` edges
    /// emitted from send / receive / close operation sites. There is one
    /// `Channel` node per alias-class, not one per operation.
    ///
    /// Qualified-name format:
    ///   * named-local:  `{package}.{containing_function}.{var_name}`
    ///   * parameter:    `{package}.{function}.{param_name}`
    ///   * struct field: `{package}.{struct_name}.{field_name}`
    ///
    /// Inserted immediately before `Other` (which carries `#[serde(other)]`
    /// and must remain the last variant). This shifts `Other`'s positional
    /// postcard discriminant, which is why the snapshot format bumps V13 to
    /// V14 with a frozen `NodeKindV13` wire mirror (see persistence §6.1).
    Channel,

    // ==================== Extensibility ====================
    /// A custom or language-specific node kind.
    ///
    /// Used for plugin-defined node types that don't fit other categories.
    /// The string identifier provides semantic meaning.
    #[serde(other)]
    Other,
}

impl NodeKind {
    /// Returns `true` if this is a callable entity (function, method).
    #[inline]
    #[must_use]
    pub const fn is_callable(self) -> bool {
        matches!(self, Self::Function | Self::Method | Self::Macro)
    }

    /// Returns `true` if this is a type definition.
    #[inline]
    #[must_use]
    pub const fn is_type_definition(self) -> bool {
        matches!(
            self,
            Self::Class | Self::Interface | Self::Trait | Self::Struct | Self::Enum | Self::Type
        )
    }

    /// Returns `true` if this is a container that can have members.
    #[inline]
    #[must_use]
    pub const fn is_container(self) -> bool {
        matches!(
            self,
            Self::Class
                | Self::Interface
                | Self::Trait
                | Self::Struct
                | Self::Module
                | Self::Enum
                | Self::StyleRule
                | Self::StyleAtRule
                | Self::JavaModule
        )
    }

    /// Returns `true` if this represents an import/export boundary.
    #[inline]
    #[must_use]
    pub const fn is_boundary(self) -> bool {
        matches!(self, Self::Import | Self::Export)
    }

    /// Returns `true` if this is a Rust lifetime pseudo-symbol.
    #[inline]
    #[must_use]
    pub const fn is_lifetime(self) -> bool {
        matches!(self, Self::Lifetime)
    }

    /// Returns the canonical string representation for serialization.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Class => "class",
            Self::Interface => "interface",
            Self::Trait => "trait",
            Self::Module => "module",
            Self::Variable => "variable",
            Self::Constant => "constant",
            Self::Type => "type",
            Self::Struct => "struct",
            Self::Enum => "enum",
            Self::EnumVariant => "enum_variant",
            Self::Macro => "macro",
            Self::Parameter => "parameter",
            Self::Property => "property",
            Self::CallSite => "call_site",
            Self::Import => "import",
            Self::Export => "export",
            Self::StyleRule => "style_rule",
            Self::StyleAtRule => "style_at_rule",
            Self::StyleVariable => "style_variable",
            Self::Lifetime => "lifetime",
            Self::Component => "component",
            Self::Service => "service",
            Self::Resource => "resource",
            Self::Endpoint => "endpoint",
            Self::Test => "test",
            Self::TypeParameter => "type_parameter",
            Self::Annotation => "annotation",
            Self::AnnotationValue => "annotation_value",
            Self::LambdaTarget => "lambda_target",
            Self::JavaModule => "java_module",
            Self::EnumConstant => "enum_constant",
            Self::Channel => "channel",
            Self::Other => "other",
        }
    }

    /// Parses a string into a `NodeKind`.
    ///
    /// Returns `None` if the string doesn't match any known kind.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "function" => Some(Self::Function),
            "method" => Some(Self::Method),
            "class" => Some(Self::Class),
            "interface" => Some(Self::Interface),
            "trait" => Some(Self::Trait),
            "module" => Some(Self::Module),
            "variable" => Some(Self::Variable),
            "constant" => Some(Self::Constant),
            "type" => Some(Self::Type),
            "struct" => Some(Self::Struct),
            "enum" => Some(Self::Enum),
            "enum_variant" => Some(Self::EnumVariant),
            "macro" => Some(Self::Macro),
            "parameter" => Some(Self::Parameter),
            "property" => Some(Self::Property),
            "call_site" => Some(Self::CallSite),
            "import" => Some(Self::Import),
            "export" => Some(Self::Export),
            "style_rule" => Some(Self::StyleRule),
            "style_at_rule" => Some(Self::StyleAtRule),
            "style_variable" => Some(Self::StyleVariable),
            "lifetime" => Some(Self::Lifetime),
            "component" => Some(Self::Component),
            "service" => Some(Self::Service),
            "resource" => Some(Self::Resource),
            "endpoint" => Some(Self::Endpoint),
            "test" => Some(Self::Test),
            "type_parameter" => Some(Self::TypeParameter),
            "annotation" => Some(Self::Annotation),
            "annotation_value" => Some(Self::AnnotationValue),
            "lambda_target" => Some(Self::LambdaTarget),
            "java_module" => Some(Self::JavaModule),
            "enum_constant" => Some(Self::EnumConstant),
            "channel" => Some(Self::Channel),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for NodeKind {
    /// Returns `NodeKind::Function` as the default (most common node type).
    fn default() -> Self {
        Self::Function
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_kind_as_str() {
        assert_eq!(NodeKind::Function.as_str(), "function");
        assert_eq!(NodeKind::Method.as_str(), "method");
        assert_eq!(NodeKind::Class.as_str(), "class");
        assert_eq!(NodeKind::CallSite.as_str(), "call_site");
        assert_eq!(NodeKind::EnumVariant.as_str(), "enum_variant");
    }

    #[test]
    fn test_node_kind_parse() {
        assert_eq!(NodeKind::parse("function"), Some(NodeKind::Function));
        assert_eq!(NodeKind::parse("call_site"), Some(NodeKind::CallSite));
        assert_eq!(NodeKind::parse("unknown"), None);
    }

    #[test]
    fn test_node_kind_display() {
        assert_eq!(format!("{}", NodeKind::Function), "function");
        assert_eq!(format!("{}", NodeKind::EnumVariant), "enum_variant");
    }

    #[test]
    fn test_is_callable() {
        assert!(NodeKind::Function.is_callable());
        assert!(NodeKind::Method.is_callable());
        assert!(NodeKind::Macro.is_callable());
        assert!(!NodeKind::Class.is_callable());
        assert!(!NodeKind::Variable.is_callable());
    }

    #[test]
    fn test_is_type_definition() {
        assert!(NodeKind::Class.is_type_definition());
        assert!(NodeKind::Interface.is_type_definition());
        assert!(NodeKind::Trait.is_type_definition());
        assert!(NodeKind::Struct.is_type_definition());
        assert!(NodeKind::Enum.is_type_definition());
        assert!(NodeKind::Type.is_type_definition());
        assert!(!NodeKind::Function.is_type_definition());
        assert!(!NodeKind::Variable.is_type_definition());
    }

    #[test]
    fn test_is_container() {
        assert!(NodeKind::Class.is_container());
        assert!(NodeKind::Module.is_container());
        assert!(NodeKind::Struct.is_container());
        assert!(!NodeKind::Function.is_container());
        assert!(!NodeKind::Variable.is_container());
    }

    #[test]
    fn test_is_boundary() {
        assert!(NodeKind::Import.is_boundary());
        assert!(NodeKind::Export.is_boundary());
        assert!(!NodeKind::Function.is_boundary());
    }

    #[test]
    fn test_is_lifetime() {
        assert!(NodeKind::Lifetime.is_lifetime());
        assert!(!NodeKind::Function.is_lifetime());
        assert!(!NodeKind::Variable.is_lifetime());
    }

    #[test]
    fn test_lifetime_serialization() {
        assert_eq!(NodeKind::Lifetime.as_str(), "lifetime");
        assert_eq!(NodeKind::parse("lifetime"), Some(NodeKind::Lifetime));

        // JSON roundtrip
        let json = serde_json::to_string(&NodeKind::Lifetime).unwrap();
        assert_eq!(json, "\"lifetime\"");
        let deserialized: NodeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, NodeKind::Lifetime);
    }

    #[test]
    fn test_default() {
        assert_eq!(NodeKind::default(), NodeKind::Function);
    }

    #[test]
    fn test_serde_roundtrip() {
        let kinds = [
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Class,
            NodeKind::CallSite,
            NodeKind::EnumVariant,
        ];

        for kind in kinds {
            // JSON roundtrip
            let json = serde_json::to_string(&kind).unwrap();
            let deserialized: NodeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, deserialized);

            // Postcard roundtrip
            let bytes = postcard::to_allocvec(&kind).unwrap();
            let deserialized: NodeKind = postcard::from_bytes(&bytes).unwrap();
            assert_eq!(kind, deserialized);
        }
    }

    #[test]
    fn test_json_format() {
        // Verify snake_case serialization
        assert_eq!(
            serde_json::to_string(&NodeKind::Function).unwrap(),
            "\"function\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::CallSite).unwrap(),
            "\"call_site\""
        );
        assert_eq!(
            serde_json::to_string(&NodeKind::EnumVariant).unwrap(),
            "\"enum_variant\""
        );
    }

    #[test]
    fn test_hash() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(NodeKind::Function);
        set.insert(NodeKind::Method);
        set.insert(NodeKind::Class);

        assert!(set.contains(&NodeKind::Function));
        assert!(!set.contains(&NodeKind::Variable));
        assert_eq!(set.len(), 3);
    }

    #[test]
    #[allow(clippy::clone_on_copy)] // Intentionally testing Clone trait
    fn test_copy_clone() {
        let kind = NodeKind::Function;
        let copied = kind;
        let cloned = kind.clone();

        assert_eq!(kind, copied);
        assert_eq!(kind, cloned);
    }
}
