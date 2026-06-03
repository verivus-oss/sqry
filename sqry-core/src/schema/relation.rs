//! Canonical relation kind enumeration.
//!
//! Defines the types of symbol relationships that can be queried.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Types of symbol relationships for relation queries.
///
/// Used by the `relation_query` tool/command to specify which
/// relationships to traverse from a given symbol.
///
/// # Serialization
///
/// All variants serialize to lowercase: `"callers"`, `"callees"`, etc.
///
/// # Examples
///
/// ```
/// use sqry_core::schema::RelationKind;
///
/// let kind = RelationKind::Callers;
/// assert_eq!(kind.as_str(), "callers");
///
/// let parsed = RelationKind::parse("callees").unwrap();
/// assert_eq!(parsed, RelationKind::Callees);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum RelationKind {
    /// Find symbols that call the target symbol.
    ///
    /// Traverses incoming `Calls` edges in the graph.
    #[default]
    Callers,

    /// Find symbols that the target symbol calls.
    ///
    /// Traverses outgoing `Calls` edges in the graph.
    Callees,

    /// Find symbols imported by the target symbol/file.
    ///
    /// Traverses `Imports` edges in the graph.
    Imports,

    /// Find symbols exported by the target symbol/file.
    ///
    /// Traverses `Exports` edges in the graph.
    Exports,

    /// Find return type relationships.
    ///
    /// Traverses `TypeOf` edges where the source is a function/method.
    Returns,

    /// Find error-chain wrap relationships (T3.6 / Cluster G).
    ///
    /// Traverses outbound `EdgeKind::Wraps` edges of any `WrapKind`
    /// (`ErrorfVerb`, `UnwrapMethod`, `UnwrapMultiMethod`,
    /// `ErrorsIs`, `ErrorsAs`, `ErrorsAsType`, `ErrorsJoin`). For
    /// kind-filtered queries use the planner's `wraps:<kind>`
    /// predicate.
    Wraps,

    /// Find channel send / receive / close operation sites on a channel
    /// (Go T2.4).
    ///
    /// Traverses `EdgeKind::ChannelPeer` edges anchored on a `Channel`
    /// node (or expanded from a containing function's body). The
    /// container-level `rename_all = "lowercase"` would serialize this as
    /// `"channelpeers"`, so an explicit per-variant rename pins the
    /// `"channel_peers"` wire string.
    #[serde(rename = "channel_peers")]
    ChannelPeers,

    /// Find generic-instantiation call sites of a generic function /
    /// method (Go T2.5).
    ///
    /// Traverses `EdgeKind::Instantiates` edges. The explicit rename is
    /// redundant (`Instantiations` already lowercases to
    /// `"instantiations"`) but kept for symmetry with `ChannelPeers`.
    #[serde(rename = "instantiations")]
    Instantiations,
}

impl RelationKind {
    /// Returns all variants in definition order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Callers,
            Self::Callees,
            Self::Imports,
            Self::Exports,
            Self::Returns,
            Self::Wraps,
            Self::ChannelPeers,
            Self::Instantiations,
        ]
    }

    /// Returns the canonical string representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Callers => "callers",
            Self::Callees => "callees",
            Self::Imports => "imports",
            Self::Exports => "exports",
            Self::Returns => "returns",
            Self::Wraps => "wraps",
            Self::ChannelPeers => "channel_peers",
            Self::Instantiations => "instantiations",
        }
    }

    /// Parses a string into a `RelationKind`.
    ///
    /// Returns `None` if the string doesn't match any known kind.
    /// Case-insensitive.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "callers" => Some(Self::Callers),
            "callees" => Some(Self::Callees),
            "imports" => Some(Self::Imports),
            "exports" => Some(Self::Exports),
            "returns" => Some(Self::Returns),
            "wraps" => Some(Self::Wraps),
            "channel_peers" => Some(Self::ChannelPeers),
            "instantiations" => Some(Self::Instantiations),
            _ => None,
        }
    }

    /// Returns `true` if this relation traverses call edges.
    #[must_use]
    pub const fn is_call_relation(self) -> bool {
        matches!(self, Self::Callers | Self::Callees)
    }

    /// Returns `true` if this relation traverses import/export edges.
    #[must_use]
    pub const fn is_boundary_relation(self) -> bool {
        matches!(self, Self::Imports | Self::Exports)
    }
}

impl fmt::Display for RelationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_as_str() {
        assert_eq!(RelationKind::Callers.as_str(), "callers");
        assert_eq!(RelationKind::Callees.as_str(), "callees");
        assert_eq!(RelationKind::Imports.as_str(), "imports");
        assert_eq!(RelationKind::Exports.as_str(), "exports");
        assert_eq!(RelationKind::Returns.as_str(), "returns");
    }

    #[test]
    fn test_parse() {
        assert_eq!(RelationKind::parse("callers"), Some(RelationKind::Callers));
        assert_eq!(RelationKind::parse("CALLEES"), Some(RelationKind::Callees));
        assert_eq!(RelationKind::parse("Imports"), Some(RelationKind::Imports));
        assert_eq!(RelationKind::parse("unknown"), None);
    }

    #[test]
    fn test_new_relation_kinds_wire_strings() {
        // The wire strings are contract-locked: ChannelPeers must NOT
        // serialize as "channelpeers" (the lowercase rename_all default).
        assert_eq!(RelationKind::ChannelPeers.as_str(), "channel_peers");
        assert_eq!(RelationKind::Instantiations.as_str(), "instantiations");
        assert_eq!(
            RelationKind::parse("channel_peers"),
            Some(RelationKind::ChannelPeers)
        );
        assert_eq!(
            RelationKind::parse("instantiations"),
            Some(RelationKind::Instantiations)
        );
        // serde wire shape (used by the MCP / CLI JSON surface).
        assert_eq!(
            serde_json::to_string(&RelationKind::ChannelPeers).unwrap(),
            "\"channel_peers\""
        );
        assert_eq!(
            serde_json::to_string(&RelationKind::Instantiations).unwrap(),
            "\"instantiations\""
        );
        // Neither new relation is a call or boundary relation.
        assert!(!RelationKind::ChannelPeers.is_call_relation());
        assert!(!RelationKind::Instantiations.is_boundary_relation());
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", RelationKind::Callers), "callers");
        assert_eq!(format!("{}", RelationKind::Returns), "returns");
    }

    #[test]
    fn test_serde_roundtrip() {
        for kind in RelationKind::all() {
            let json = serde_json::to_string(kind).unwrap();
            let deserialized: RelationKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, deserialized);
        }
    }

    #[test]
    fn test_classification() {
        assert!(RelationKind::Callers.is_call_relation());
        assert!(RelationKind::Callees.is_call_relation());
        assert!(!RelationKind::Imports.is_call_relation());

        assert!(RelationKind::Imports.is_boundary_relation());
        assert!(RelationKind::Exports.is_boundary_relation());
        assert!(!RelationKind::Callers.is_boundary_relation());
    }
}
