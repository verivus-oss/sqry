//! Canonical unused scope enumeration.
//!
//! Defines scopes for unused symbol detection.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Scopes for unused symbol detection.
///
/// Used by `find_unused` tool to specify which category of
/// potentially unused symbols to search for.
///
/// # Serialization
///
/// All variants serialize to lowercase: `"public"`, `"all"`, etc.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::UnusedScope;
///
/// let scope = UnusedScope::Function;
/// assert_eq!(scope.as_str(), "function");
///
/// let parsed = UnusedScope::parse("private").unwrap();
/// assert_eq!(parsed, UnusedScope::Private);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum UnusedScope {
    /// Public symbols only.
    ///
    /// Finds public/exported symbols that have no external references.
    /// These are candidates for API cleanup.
    Public,

    /// Private symbols only.
    ///
    /// Finds private/internal symbols that have no references.
    /// These are likely dead code that can be safely removed.
    Private,

    /// Functions/methods only.
    ///
    /// Finds unreferenced functions and methods across all visibilities.
    Function,

    /// Structs/classes only.
    ///
    /// Finds unreferenced type definitions.
    Struct,

    /// All symbols (default).
    ///
    /// Searches all symbol types and visibilities for unused code.
    #[default]
    All,
}

impl UnusedScope {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Public,
            Self::Private,
            Self::Function,
            Self::Struct,
            Self::All,
        ]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Function => "function",
            Self::Struct => "struct",
            Self::All => "all",
        }
    }

    /// Parses a string into an `UnusedScope`.
    ///
    /// Returns `None` if the string doesn't match any known scope.
    /// Case-insensitive.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "public" | "pub" | "exported" => Some(Self::Public),
            "private" | "priv" | "internal" => Some(Self::Private),
            "function" | "func" | "method" => Some(Self::Function),
            "struct" | "class" | "type" => Some(Self::Struct),
            "all" | "*" => Some(Self::All),
            _ => None,
        }
    }

    /// Returns `true` if this scope filters by visibility.
    #[must_use]
    pub const fn is_visibility_filter(self) -> bool {
        matches!(self, Self::Public | Self::Private)
    }

    /// Returns `true` if this scope filters by symbol kind.
    #[must_use]
    pub const fn is_kind_filter(self) -> bool {
        matches!(self, Self::Function | Self::Struct)
    }

    /// Returns a human-readable description of this scope.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Public => "unused public/exported symbols",
            Self::Private => "unused private/internal symbols",
            Self::Function => "unused functions and methods",
            Self::Struct => "unused structs and classes",
            Self::All => "all unused symbols",
        }
    }
}

impl fmt::Display for UnusedScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(UnusedScope::Public.as_str(), "public");
        assert_eq!(UnusedScope::Private.as_str(), "private");
        assert_eq!(UnusedScope::Function.as_str(), "function");
        assert_eq!(UnusedScope::Struct.as_str(), "struct");
        assert_eq!(UnusedScope::All.as_str(), "all");
    }

    #[test]
    fn test_parse() {
        assert_eq!(UnusedScope::parse("public"), Some(UnusedScope::Public));
        assert_eq!(UnusedScope::parse("PRIVATE"), Some(UnusedScope::Private));
        assert_eq!(UnusedScope::parse("func"), Some(UnusedScope::Function));
        assert_eq!(UnusedScope::parse("class"), Some(UnusedScope::Struct));
        assert_eq!(UnusedScope::parse("*"), Some(UnusedScope::All));
        assert_eq!(UnusedScope::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", UnusedScope::Public), "public");
        assert_eq!(format!("{}", UnusedScope::All), "all");
    }

    #[test]
    fn test_serde_roundtrip() {
        for scope in UnusedScope::all() {
            let json = serde_json::to_string(scope).unwrap();
            let deserialized: UnusedScope = serde_json::from_str(&json).unwrap();
            assert_eq!(*scope, deserialized);
        }
    }

    #[test]
    fn test_default() {
        assert_eq!(UnusedScope::default(), UnusedScope::All);
    }

    #[test]
    fn test_classification() {
        assert!(UnusedScope::Public.is_visibility_filter());
        assert!(UnusedScope::Private.is_visibility_filter());
        assert!(!UnusedScope::Function.is_visibility_filter());

        assert!(UnusedScope::Function.is_kind_filter());
        assert!(UnusedScope::Struct.is_kind_filter());
        assert!(!UnusedScope::Public.is_kind_filter());

        assert!(!UnusedScope::All.is_visibility_filter());
        assert!(!UnusedScope::All.is_kind_filter());
    }

    #[test]
    fn test_description() {
        assert!(UnusedScope::Public.description().contains("public"));
        assert!(UnusedScope::Private.description().contains("private"));
        assert!(UnusedScope::Function.description().contains("function"));
        assert!(UnusedScope::Struct.description().contains("struct"));
        assert!(UnusedScope::All.description().contains("all"));
    }
}
