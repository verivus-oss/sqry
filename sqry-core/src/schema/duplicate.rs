//! Canonical duplicate kind enumeration.
//!
//! Defines types of duplicates to detect in code.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Types of duplicates to detect in code.
///
/// Used by `find_duplicates` tool to specify which kind of
/// code duplication to search for.
///
/// # Serialization
///
/// All variants serialize to lowercase: `"body"`, `"signature"`, etc.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::DuplicateKind;
///
/// let kind = DuplicateKind::Signature;
/// assert_eq!(kind.as_str(), "signature");
///
/// let parsed = DuplicateKind::parse("struct").unwrap();
/// assert_eq!(parsed, DuplicateKind::Struct);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum DuplicateKind {
    /// Function/method body duplicates.
    ///
    /// Finds functions with similar or identical implementations.
    /// Useful for identifying copy-paste code that could be refactored.
    #[default]
    Body,

    /// Function signature duplicates.
    ///
    /// Finds functions with identical signatures (name, parameters, return type).
    /// Useful for identifying potential interface inconsistencies.
    Signature,

    /// Struct/class definition duplicates.
    ///
    /// Finds types with similar field/property structures.
    /// Useful for identifying redundant type definitions.
    Struct,
}

impl DuplicateKind {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Body, Self::Signature, Self::Struct]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Body => "body",
            Self::Signature => "signature",
            Self::Struct => "struct",
        }
    }

    /// Parses a string into a `DuplicateKind`.
    ///
    /// Returns `None` if the string doesn't match any known kind.
    /// Case-insensitive.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "body" | "implementation" | "impl" => Some(Self::Body),
            "signature" | "sig" => Some(Self::Signature),
            "struct" | "class" | "type" => Some(Self::Struct),
            _ => None,
        }
    }

    /// Returns a human-readable description of this duplicate kind.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Body => "function/method body duplicates",
            Self::Signature => "function signature duplicates",
            Self::Struct => "struct/class definition duplicates",
        }
    }
}

impl fmt::Display for DuplicateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(DuplicateKind::Body.as_str(), "body");
        assert_eq!(DuplicateKind::Signature.as_str(), "signature");
        assert_eq!(DuplicateKind::Struct.as_str(), "struct");
    }

    #[test]
    fn test_parse() {
        assert_eq!(DuplicateKind::parse("body"), Some(DuplicateKind::Body));
        assert_eq!(
            DuplicateKind::parse("SIGNATURE"),
            Some(DuplicateKind::Signature)
        );
        assert_eq!(
            DuplicateKind::parse("implementation"),
            Some(DuplicateKind::Body)
        );
        assert_eq!(DuplicateKind::parse("class"), Some(DuplicateKind::Struct));
        assert_eq!(DuplicateKind::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", DuplicateKind::Body), "body");
        assert_eq!(format!("{}", DuplicateKind::Struct), "struct");
    }

    #[test]
    fn test_serde_roundtrip() {
        for kind in DuplicateKind::all() {
            let json = serde_json::to_string(kind).unwrap();
            let deserialized: DuplicateKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, deserialized);
        }
    }

    #[test]
    fn test_default() {
        assert_eq!(DuplicateKind::default(), DuplicateKind::Body);
    }

    #[test]
    fn test_description() {
        assert!(DuplicateKind::Body.description().contains("body"));
        assert!(DuplicateKind::Signature.description().contains("signature"));
        assert!(DuplicateKind::Struct.description().contains("struct"));
    }
}
