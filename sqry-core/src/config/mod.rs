//! Configuration module for sqry.
//!
//! This module contains all configuration-related functionality,
//! including buffer sizes, tuning parameters, and environment variable overrides.
//!
//! # Project Configuration
//!
//! Project-level configuration is loaded from `.sqry-config.toml` files.
//! See [`crate::config::ProjectConfig`] for details.
//!
//! # Configuration Snapshots
//!
//! The [`snapshot`](crate::config::snapshot) module captures effective configuration for embedding
//! into the CodeGraph, enabling provenance tracking and reproducibility.
//! See [`ConfigSnapshot`](crate::config::snapshot::ConfigSnapshot) for details.

pub mod buffers;
pub mod graph_config_persistence;
pub mod graph_config_schema;
pub mod graph_config_store;
pub mod migration;
pub mod project_config;
pub mod recursion;
pub mod snapshot;
pub mod workspace;

pub use graph_config_persistence::{
    ConfigPersistence, IntegrityStatus, LoadReport, LockInfo, PersistenceError, PersistenceResult,
    RepairReport, SchemaStatus,
};

pub use graph_config_schema::{
    AliasEntry, BuffersConfig, CliPreferences, DurabilityConfig, GraphConfig,
    GraphConfigExtensions, GraphConfigFile, GraphConfigIntegrity, GraphConfigMetadata,
    LimitsConfig, LockingConfig, OutputConfig, ParallelismConfig, PersistenceConfig,
    SCHEMA_VERSION, SchemaValidationError, TimeoutsConfig, ValidationConfig, ValidationResult,
    WrittenByInfo,
};

pub use graph_config_store::{
    GraphConfigError, GraphConfigPaths, GraphConfigStore, Result as GraphConfigResult,
};

pub use project_config::{
    CONFIG_FILE_NAME, CacheConfig, ConfigError, IgnoreConfig, IncludeConfig, IndexingConfig,
    LanguageConfig, ProjectConfig,
};

pub use snapshot::{
    CONFIG_INVENTORY, CONFIG_PROVENANCE_FILENAME, CONFIG_SCHEMA_VERSION, ConfigEntry,
    ConfigProvenance, ConfigRisk, ConfigScope, ConfigSnapshot, ConfigSnapshotBuilder, ConfigSource,
    collect_snapshot, validate_completeness,
};

pub use migration::{
    MigrationError, MigrationReport, MigrationResult, detect_legacy_config,
    is_new_config_initialized, log_deprecation_warning_if_legacy_exists, migrate_legacy_config,
};

pub use recursion::RecursionLimits;

pub use workspace::WorkspaceConfig;

// ---------------------------------------------------------------------------
// STEP_11_4 — workspace config fingerprint
// ---------------------------------------------------------------------------

/// Compute the deterministic 64-bit fingerprint that distinguishes
/// otherwise-identical workspace cache entries when the user-visible
/// configuration that materially affects graph contents changes.
///
/// The fingerprint is the first 8 bytes (little-endian) of a BLAKE3-256
/// digest over three independent inputs, fed in a canonical order with
/// length-prefixed framing so reordering or truncating any input changes
/// the digest:
///
/// 1. `plugin_selection_canonical` — canonical byte representation of
///    the active [`sqry_plugin_registry::PluginSelectionConfig`] (the
///    caller is responsible for canonical encoding; postcard /
///    serde-json with a deterministic key order both work). The input
///    must include the [`sqry_plugin_registry::HighCostMode`] discriminant
///    so high-cost-mode toggles flip the fingerprint.
/// 2. `indexing_config_canonical` — canonical byte representation of
///    the global [`crate::config::IndexingConfig`] section (recursion
///    limits, language toggles, ignore patterns, …).
/// 3. A 16-byte salt `b"sqry-cfg-fp-v1\0\0"` — bumped if the fingerprint
///    encoding ever needs an incompatible revision.
///
/// The truncation to 64 bits matches the `WorkspaceKey.config_fingerprint`
/// wire field (an unchanged fingerprint authorises the daemon to reuse
/// the cached graph; collisions only cost performance, never
/// correctness, because the canonical inputs are also serialised into
/// the persisted manifest and re-validated on load).
///
/// `plugin_selection_canonical` and `indexing_config_canonical` are
/// passed as opaque byte slices to keep `sqry-core` free of a
/// dependency on `sqry-plugin-registry` (which itself depends on
/// `sqry-core`, so the inverse import would create a cycle). Callers
/// that already hold structured `PluginSelectionConfig` /
/// `IndexingConfig` values should serialise them through `postcard` or
/// `serde_json::to_vec_pretty` (with `BTreeMap` for deterministic key
/// order) before invoking this helper.
#[must_use]
pub fn compute_workspace_config_fingerprint(
    plugin_selection_canonical: &[u8],
    indexing_config_canonical: &[u8],
) -> u64 {
    const SALT: &[u8; 16] = b"sqry-cfg-fp-v1\0\0";
    let mut hasher = blake3::Hasher::new();
    hasher.update(SALT);
    let plugin_len = u32::try_from(plugin_selection_canonical.len()).unwrap_or(u32::MAX);
    hasher.update(&plugin_len.to_le_bytes());
    hasher.update(plugin_selection_canonical);
    let indexing_len = u32::try_from(indexing_config_canonical.len()).unwrap_or(u32::MAX);
    hasher.update(&indexing_len.to_le_bytes());
    hasher.update(indexing_config_canonical);
    let digest = hasher.finalize();
    let bytes = digest.as_bytes();
    let mut prefix = [0u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(prefix)
}

#[cfg(test)]
mod fingerprint_tests {
    use super::compute_workspace_config_fingerprint;

    #[test]
    fn fingerprint_is_deterministic() {
        let a = compute_workspace_config_fingerprint(b"plugins:rust,go", b"indexing:default");
        let b = compute_workspace_config_fingerprint(b"plugins:rust,go", b"indexing:default");
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_varies_with_plugin_input() {
        let a = compute_workspace_config_fingerprint(b"plugins:rust,go", b"indexing:default");
        let b =
            compute_workspace_config_fingerprint(b"plugins:rust,go,python", b"indexing:default");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_varies_with_indexing_input() {
        let a = compute_workspace_config_fingerprint(b"plugins:rust,go", b"indexing:default");
        let b = compute_workspace_config_fingerprint(b"plugins:rust,go", b"indexing:strict");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_distinguishes_swap_of_inputs() {
        // The length-prefixed framing must prevent ambiguity between
        // `(a, b)` and `(b, a)` even when the concatenation happens to
        // align byte-wise.
        let a = compute_workspace_config_fingerprint(b"AAAA", b"BBBB");
        let b = compute_workspace_config_fingerprint(b"BBBB", b"AAAA");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_empty_inputs_is_deterministic_nonzero() {
        let a = compute_workspace_config_fingerprint(&[], &[]);
        let b = compute_workspace_config_fingerprint(&[], &[]);
        assert_eq!(a, b);
        // The salt alone produces a deterministic non-zero hash so
        // empty / unset config does not collapse to fingerprint 0
        // (which is the default-init value of WorkspaceKey, and
        // would otherwise mean "fingerprint not set").
        assert_ne!(a, 0);
    }

    #[test]
    fn fingerprint_distinguishes_high_cost_mode_toggle() {
        // Codex iter1 MINOR — the doc comment says HighCostMode is
        // part of the canonical input, so a toggle MUST flip the
        // fingerprint. Simulate the canonical encoding by including
        // a stable discriminant string for each mode.
        let fast = compute_workspace_config_fingerprint(
            b"high_cost_mode=fast_path_default;plugins=rust,go",
            b"indexing:default",
        );
        let include_all = compute_workspace_config_fingerprint(
            b"high_cost_mode=include_all;plugins=rust,go",
            b"indexing:default",
        );
        let exclude_all = compute_workspace_config_fingerprint(
            b"high_cost_mode=exclude_all;plugins=rust,go",
            b"indexing:default",
        );
        assert_ne!(fast, include_all);
        assert_ne!(fast, exclude_all);
        assert_ne!(include_all, exclude_all);
    }
}
