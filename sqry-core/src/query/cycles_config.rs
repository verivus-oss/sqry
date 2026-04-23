//! Configuration types for cycle detection.
//!
//! These types configure cycle-detection queries. The actual cycle detection
//! is implemented in `sqry-db` via
//! [`sqry_db::queries::CyclesQuery`](../../../sqry_db/queries/struct.CyclesQuery.html)
//! and [`sqry_db::queries::IsInCycleQuery`](../../../sqry_db/queries/struct.IsInCycleQuery.html),
//! which consume [`CircularType`] directly and translate [`CircularConfig`]
//! into [`sqry_db::queries::CycleBounds`](../../../sqry_db/queries/struct.CycleBounds.html).
//!
//! The legacy `find_all_cycles_graph` / `is_node_in_cycle` functions were
//! removed in Phase 3C DB19 (2026-04-15) — all cycle detection routes
//! through sqry-db so one Tarjan SCC pass is shared across MCP / CLI / LSP
//! callers on the same snapshot. The types below remain because they are
//! consumed by `sqry-db` as public key fragments.

/// Type of circular dependency to detect.
// Serialize/Deserialize added for PN3 cold-start persistence: CircularType is
// used as a field in sqry-db CyclesKey / IsInCycleKey, which are postcard-serialized
// at cache-insert time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum CircularType {
    /// Function/method call cycles (A calls B calls C calls A).
    Calls,
    /// File import cycles (a.rs imports b.rs imports a.rs).
    Imports,
    /// Module-level cycles (aggregated from import graph).
    Modules,
}

impl CircularType {
    /// Parse circular type from query value string.
    ///
    /// Accepts the canonical plural form (`calls`, `imports`, `modules`) and
    /// the legacy singular aliases (`call`, `import`, `module`).
    #[must_use]
    pub fn try_parse(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "calls" | "call" => Some(Self::Calls),
            "imports" | "import" => Some(Self::Imports),
            "modules" | "module" => Some(Self::Modules),
            _ => None,
        }
    }
}

/// Configuration for circular dependency detection.
///
/// Mirrors the sqry-db [`sqry_db::queries::CycleBounds`](../../../sqry_db/queries/struct.CycleBounds.html)
/// type. Handlers construct a `CircularConfig` from user CLI/MCP flags and
/// pass it through to the sqry-db key.
#[derive(Debug, Clone)]
pub struct CircularConfig {
    /// Minimum cycle depth to report (default: 2).
    pub min_depth: usize,
    /// Maximum cycle depth to report (default: unbounded).
    pub max_depth: Option<usize>,
    /// Maximum results to return (default: 100).
    pub max_results: usize,
    /// Include self-loops (A -> A) in results (default: false).
    pub should_include_self_loops: bool,
}

impl Default for CircularConfig {
    fn default() -> Self {
        Self {
            min_depth: 2,
            max_depth: None,
            max_results: 100,
            should_include_self_loops: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_plural_forms() {
        assert_eq!(CircularType::try_parse("calls"), Some(CircularType::Calls));
        assert_eq!(
            CircularType::try_parse("imports"),
            Some(CircularType::Imports)
        );
        assert_eq!(
            CircularType::try_parse("modules"),
            Some(CircularType::Modules)
        );
    }

    #[test]
    fn parses_legacy_singular_aliases() {
        assert_eq!(CircularType::try_parse("call"), Some(CircularType::Calls));
        assert_eq!(
            CircularType::try_parse("import"),
            Some(CircularType::Imports)
        );
        assert_eq!(
            CircularType::try_parse("module"),
            Some(CircularType::Modules)
        );
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(CircularType::try_parse("CALLS"), Some(CircularType::Calls));
        assert_eq!(
            CircularType::try_parse("Imports"),
            Some(CircularType::Imports)
        );
    }

    #[test]
    fn rejects_unknown_types() {
        assert_eq!(CircularType::try_parse("unknown"), None);
        assert_eq!(CircularType::try_parse(""), None);
    }

    #[test]
    fn default_config_matches_legacy() {
        let config = CircularConfig::default();
        assert_eq!(config.min_depth, 2);
        assert_eq!(config.max_depth, None);
        assert_eq!(config.max_results, 100);
        assert!(!config.should_include_self_loops);
    }
}
