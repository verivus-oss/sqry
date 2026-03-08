//! Canonical cycle kind enumeration.
//!
//! Defines types of cycles to detect in code graphs.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Types of cycles to detect in code graphs.
///
/// Used by `find_cycles` and `is_node_in_cycle` tools to specify
/// which type of cyclic dependencies to find.
///
/// # Serialization
///
/// All variants serialize to lowercase: `"calls"`, `"imports"`, etc.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::CycleKind;
///
/// let kind = CycleKind::Imports;
/// assert_eq!(kind.as_str(), "imports");
///
/// let parsed = CycleKind::parse("modules").unwrap();
/// assert_eq!(parsed, CycleKind::Modules);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum CycleKind {
    /// Call cycles (A calls B, B calls A).
    ///
    /// Detects recursive call patterns that may indicate
    /// infinite recursion or circular dependencies.
    #[default]
    Calls,

    /// Import cycles (A imports B, B imports A).
    ///
    /// Detects circular import dependencies that can cause
    /// initialization order problems in many languages.
    Imports,

    /// Module-level cycles (module A depends on module B, B depends on A).
    ///
    /// Higher-level view of circular dependencies at the
    /// module/package level rather than individual symbols.
    Modules,
}

impl CycleKind {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[Self::Calls, Self::Imports, Self::Modules]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Imports => "imports",
            Self::Modules => "modules",
        }
    }

    /// Parses a string into a `CycleKind`.
    ///
    /// Returns `None` if the string doesn't match any known kind.
    /// Case-insensitive.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "calls" | "call" | "function" => Some(Self::Calls),
            "imports" | "import" => Some(Self::Imports),
            "modules" | "module" | "package" => Some(Self::Modules),
            _ => None,
        }
    }

    /// Returns a human-readable description of this cycle kind.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Calls => "function/method call cycles",
            Self::Imports => "import/dependency cycles",
            Self::Modules => "module-level dependency cycles",
        }
    }
}

impl fmt::Display for CycleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(CycleKind::Calls.as_str(), "calls");
        assert_eq!(CycleKind::Imports.as_str(), "imports");
        assert_eq!(CycleKind::Modules.as_str(), "modules");
    }

    #[test]
    fn test_parse() {
        assert_eq!(CycleKind::parse("calls"), Some(CycleKind::Calls));
        assert_eq!(CycleKind::parse("IMPORTS"), Some(CycleKind::Imports));
        assert_eq!(CycleKind::parse("call"), Some(CycleKind::Calls));
        assert_eq!(CycleKind::parse("package"), Some(CycleKind::Modules));
        assert_eq!(CycleKind::parse("unknown"), None);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", CycleKind::Calls), "calls");
        assert_eq!(format!("{}", CycleKind::Modules), "modules");
    }

    #[test]
    fn test_serde_roundtrip() {
        for kind in CycleKind::all() {
            let json = serde_json::to_string(kind).unwrap();
            let deserialized: CycleKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, deserialized);
        }
    }

    #[test]
    fn test_default() {
        assert_eq!(CycleKind::default(), CycleKind::Calls);
    }

    #[test]
    fn test_description() {
        assert!(CycleKind::Calls.description().contains("call"));
        assert!(CycleKind::Imports.description().contains("import"));
        assert!(CycleKind::Modules.description().contains("module"));
    }
}
