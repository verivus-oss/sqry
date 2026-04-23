//! Configuration type for unused/dead-code detection.
//!
//! The actual unused-symbol detection is implemented in `sqry-db` via
//! [`sqry_db::queries::UnusedQuery`](../../../sqry_db/queries/struct.UnusedQuery.html),
//! which consumes [`UnusedScope`] directly via
//! [`sqry_db::queries::UnusedKey`](../../../sqry_db/queries/struct.UnusedKey.html).
//!
//! The legacy `find_unused_nodes` / `compute_reachable_set_graph` /
//! `is_node_unused` functions were removed in Phase 3C DB19 (2026-04-15)
//! — all unused detection routes through sqry-db so one reachability pass
//! is shared across MCP / CLI / LSP callers on the same snapshot. The
//! [`UnusedScope`] enum remains because it is consumed by `sqry-db` as a
//! public key fragment and by MCP / CLI / LSP handlers as the parsed
//! user-facing scope value.

/// Scope for unused analysis.
///
/// Determines which symbols are candidates for unused detection.
///
/// # MCP vs sqry-db semantics
///
/// The sqry-db [`UnusedQuery`](../../../sqry_db/queries/struct.UnusedQuery.html)
/// uses this enum as its raw scope filter. Handlers that want stricter or
/// broader semantics (e.g. MCP's `Struct` variant which additionally
/// includes `Interface | Trait`) must pass a provable *superset* into the
/// sqry-db key and post-filter on return. See
/// `sqry_mcp::execution::tools::analysis::mcp_scope_to_core_superset` for
/// the authoritative audit table.
// Serialize/Deserialize added for PN3 cold-start persistence: UnusedScope is
// used as a field in sqry-db UnusedKey / IsNodeUnusedKey, which are
// postcard-serialized at cache-insert time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum UnusedScope {
    /// Public symbols with no external references.
    Public,
    /// Private symbols with no references.
    Private,
    /// Unused functions (any visibility).
    Function,
    /// Unused structs/types (any visibility).
    Struct,
    /// All unused symbols.
    All,
}

impl UnusedScope {
    /// Parse scope from query value string.
    ///
    /// Case-insensitive. The empty string parses as [`Self::All`] to match
    /// the legacy `graph_unused::UnusedScope::try_parse` behavior before
    /// DB19.
    #[must_use]
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "public" => Some(Self::Public),
            "private" => Some(Self::Private),
            "function" => Some(Self::Function),
            "struct" => Some(Self::Struct),
            "all" | "" => Some(Self::All),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_variants() {
        assert_eq!(UnusedScope::try_parse("public"), Some(UnusedScope::Public));
        assert_eq!(
            UnusedScope::try_parse("private"),
            Some(UnusedScope::Private)
        );
        assert_eq!(
            UnusedScope::try_parse("function"),
            Some(UnusedScope::Function)
        );
        assert_eq!(UnusedScope::try_parse("struct"), Some(UnusedScope::Struct));
        assert_eq!(UnusedScope::try_parse("all"), Some(UnusedScope::All));
    }

    #[test]
    fn empty_string_is_all() {
        assert_eq!(UnusedScope::try_parse(""), Some(UnusedScope::All));
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(UnusedScope::try_parse("PUBLIC"), Some(UnusedScope::Public));
        assert_eq!(
            UnusedScope::try_parse("Function"),
            Some(UnusedScope::Function)
        );
    }

    #[test]
    fn rejects_unknown_scopes() {
        assert_eq!(UnusedScope::try_parse("unknown"), None);
        assert_eq!(UnusedScope::try_parse("class"), None);
    }
}
