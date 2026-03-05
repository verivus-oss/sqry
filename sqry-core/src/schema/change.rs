//! Canonical change kind enumeration.
//!
//! Defines types of changes detected by semantic diff.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Types of changes detected by semantic diff.
///
/// Used by `semantic_diff` to categorize differences between
/// two versions of a codebase.
///
/// # Serialization
///
/// All variants serialize to `snake_case`: `"added"`, `"signature_changed"`, etc.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::ChangeKind;
///
/// let kind = ChangeKind::SignatureChanged;
/// assert_eq!(kind.as_str(), "signature_changed");
///
/// let parsed = ChangeKind::parse("modified").unwrap();
/// assert_eq!(parsed, ChangeKind::Modified);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Node was added (exists in target, not in base).
    Added,

    /// Node was removed (exists in base, not in target).
    Removed,

    /// Node was modified (implementation changed, signature same).
    Modified,

    /// Node was renamed (same implementation, different name).
    Renamed,

    /// Node signature changed (parameters, return type, etc.).
    SignatureChanged,
}

impl ChangeKind {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Added,
            Self::Removed,
            Self::Modified,
            Self::Renamed,
            Self::SignatureChanged,
        ]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::SignatureChanged => "signature_changed",
        }
    }

    /// Parses a string into a `ChangeKind`.
    ///
    /// Returns `None` if the string doesn't match any known kind.
    /// Case-insensitive, accepts both `snake_case` and lowercase.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().replace('-', "_").as_str() {
            "added" | "new" => Some(Self::Added),
            "removed" | "deleted" => Some(Self::Removed),
            "modified" | "changed" => Some(Self::Modified),
            "renamed" => Some(Self::Renamed),
            "signature_changed" | "signaturechanged" => Some(Self::SignatureChanged),
            _ => None,
        }
    }

    /// Returns `true` if this is a structural change (added/removed).
    #[must_use]
    pub const fn is_structural(self) -> bool {
        matches!(self, Self::Added | Self::Removed)
    }

    /// Returns `true` if this is a content change (modified/renamed/signature).
    #[must_use]
    pub const fn is_content_change(self) -> bool {
        matches!(
            self,
            Self::Modified | Self::Renamed | Self::SignatureChanged
        )
    }

    /// Returns `true` if this change affects the public API.
    #[must_use]
    pub const fn affects_api(self) -> bool {
        matches!(
            self,
            Self::Added | Self::Removed | Self::Renamed | Self::SignatureChanged
        )
    }
}

impl fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Default for ChangeKind {
    fn default() -> Self {
        Self::Modified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(ChangeKind::Added.as_str(), "added");
        assert_eq!(ChangeKind::Removed.as_str(), "removed");
        assert_eq!(ChangeKind::Modified.as_str(), "modified");
        assert_eq!(ChangeKind::Renamed.as_str(), "renamed");
        assert_eq!(ChangeKind::SignatureChanged.as_str(), "signature_changed");
    }

    #[test]
    fn test_parse() {
        assert_eq!(ChangeKind::parse("added"), Some(ChangeKind::Added));
        assert_eq!(ChangeKind::parse("REMOVED"), Some(ChangeKind::Removed));
        assert_eq!(ChangeKind::parse("new"), Some(ChangeKind::Added));
        assert_eq!(ChangeKind::parse("deleted"), Some(ChangeKind::Removed));
        assert_eq!(
            ChangeKind::parse("signature_changed"),
            Some(ChangeKind::SignatureChanged)
        );
        assert_eq!(ChangeKind::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", ChangeKind::Added), "added");
        assert_eq!(
            format!("{}", ChangeKind::SignatureChanged),
            "signature_changed"
        );
    }

    #[test]
    fn test_serde_roundtrip() {
        for kind in ChangeKind::all() {
            let json = serde_json::to_string(kind).unwrap();
            let deserialized: ChangeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, deserialized);
        }
    }

    #[test]
    fn test_classification() {
        assert!(ChangeKind::Added.is_structural());
        assert!(ChangeKind::Removed.is_structural());
        assert!(!ChangeKind::Modified.is_structural());

        assert!(ChangeKind::Modified.is_content_change());
        assert!(ChangeKind::Renamed.is_content_change());
        assert!(!ChangeKind::Added.is_content_change());

        assert!(ChangeKind::Added.affects_api());
        assert!(ChangeKind::Removed.affects_api());
        assert!(ChangeKind::Renamed.affects_api());
        assert!(ChangeKind::SignatureChanged.affects_api());
        assert!(!ChangeKind::Modified.affects_api());
    }
}
