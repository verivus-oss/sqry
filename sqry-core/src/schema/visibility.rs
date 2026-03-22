//! Canonical visibility enumeration.
//!
//! Defines symbol visibility levels for filtering search results.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Node visibility levels for filtering.
///
/// Used to filter search results by the visibility/accessibility
/// of symbols in the codebase.
///
/// # Serialization
///
/// All variants serialize to lowercase: `"public"`, `"private"`.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::Visibility;
///
/// let vis = Visibility::Public;
/// assert_eq!(vis.as_str(), "public");
///
/// let parsed = Visibility::parse("private").unwrap();
/// assert_eq!(parsed, Visibility::Private);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Visibility {
    /// Public symbols (exported, accessible outside defining module).
    ///
    /// Includes: `pub`, `export`, `public`, etc. depending on language.
    #[default]
    Public,

    /// Private symbols (not exported, internal to defining module).
    ///
    /// Includes: no modifier (Rust), `private`, internal, etc.
    Private,
}

impl Visibility {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Public, Self::Private]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }

    /// Parses a string into a `Visibility`.
    ///
    /// Returns `None` if the string doesn't match any known visibility.
    /// Case-insensitive.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "public" | "pub" | "exported" => Some(Self::Public),
            "private" | "priv" | "internal" => Some(Self::Private),
            _ => None,
        }
    }

    /// Returns `true` if this is public visibility.
    #[must_use]
    pub const fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }

    /// Returns `true` if this is private visibility.
    #[must_use]
    pub const fn is_private(self) -> bool {
        matches!(self, Self::Private)
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(Visibility::Public.as_str(), "public");
        assert_eq!(Visibility::Private.as_str(), "private");
    }

    #[test]
    fn test_parse() {
        assert_eq!(Visibility::parse("public"), Some(Visibility::Public));
        assert_eq!(Visibility::parse("PRIVATE"), Some(Visibility::Private));
        assert_eq!(Visibility::parse("pub"), Some(Visibility::Public));
        assert_eq!(Visibility::parse("internal"), Some(Visibility::Private));
        assert_eq!(Visibility::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", Visibility::Public), "public");
        assert_eq!(format!("{}", Visibility::Private), "private");
    }

    #[test]
    fn test_serde_roundtrip() {
        for vis in Visibility::all() {
            let json = serde_json::to_string(vis).unwrap();
            let deserialized: Visibility = serde_json::from_str(&json).unwrap();
            assert_eq!(*vis, deserialized);
        }
    }

    #[test]
    fn test_classification() {
        assert!(Visibility::Public.is_public());
        assert!(!Visibility::Public.is_private());
        assert!(Visibility::Private.is_private());
        assert!(!Visibility::Private.is_public());
    }
}
