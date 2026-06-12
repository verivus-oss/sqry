//! Configuration for `QueryDb`.
//!
//! All settings have sensible defaults. Override via [`QueryDbConfig::builder`]
//! or environment variables when running as MCP/CLI.

/// Configuration for the incremental query database.
#[derive(Debug, Clone)]
pub struct QueryDbConfig {
    /// Number of cache shards. Must be a power of two. Default: 64.
    pub shard_count: usize,
    /// Arena fragmentation threshold for triggering compaction (0.0–1.0).
    /// Default: 0.20 (20%).
    pub compaction_fragmentation_threshold: f64,
    /// Delta buffer ratio threshold for triggering compaction (0.0–1.0).
    /// Default: 0.10 (10%).
    pub compaction_delta_ratio_threshold: f64,
    /// Path for persisted derived facts (relative to `.sqry/graph/`).
    /// Default: `"derived.sqry"`.
    pub derived_persistence_filename: String,
    /// Whether to enable async background compaction. Default: true.
    pub enable_background_compaction: bool,
    /// Maximum bytes per cached entry (raw key+value postcard encoding).
    ///
    /// Entries whose serialized size exceeds this cap are evaluated normally
    /// but **NOT cached**. This prevents a single oversized result from
    /// evicting many smaller entries and wasting shard memory.
    ///
    /// Default: 1 MiB (`1_048_576` bytes = `1 << 20`).
    pub max_entry_size_bytes: usize,

    /// Test-only knob: disables planner fusion. Default: `false` (fusion
    /// enabled).
    ///
    /// **Gating.** The field is compiled only under `cfg(any(test, feature =
    /// "baseline"))` so production binaries — release `sqry`, `sqry-mcp`,
    /// `sqry-lsp`, `sqryd` — never carry it. This matches the gating
    /// strategy used for [`crate::baseline`] (the oracle module for WS1
    /// differential tests) and keeps production wire-compatibility
    /// unchanged.
    ///
    /// **Semantics.** Currently consulted exclusively by the WS1 fusion
    /// equivalence property test
    /// (`sqry-db/tests/property/fusion_equivalence.rs`, DAG unit
    /// `U_WS1_9_FUSION`). The flag is not yet consulted by production
    /// planner dispatch paths — the equivalence harness runs the fused
    /// (`execute_batch` after `fuse_single`) and unfused (`execute_plan`)
    /// paths side-by-side regardless of this field, and uses
    /// [`Self::disable_fusion`] only to assemble the comparison-side
    /// `QueryDb` so future production hookups have a single config
    /// surface to branch on.
    ///
    /// **Why a config field rather than just two test entry points?**
    /// DAG `critical_decisions` line for `U_WS1_9_FUSION` reads
    /// "`disable_fusion` exposed as public test-only config" — having the
    /// knob on `QueryDbConfig` gives WS1 (and any future regression
    /// harness) a single point of control, and makes accidental
    /// regression of the field into production builds visible to the
    /// `nm | grep disable_fusion` verification gate.
    #[cfg(any(test, feature = "baseline"))]
    pub fusion_disabled: bool,
}

impl Default for QueryDbConfig {
    fn default() -> Self {
        Self {
            shard_count: 64,
            compaction_fragmentation_threshold: 0.20,
            compaction_delta_ratio_threshold: 0.10,
            derived_persistence_filename: "derived.sqry".to_owned(),
            enable_background_compaction: true,
            max_entry_size_bytes: 1 << 20, // 1 MiB
            #[cfg(any(test, feature = "baseline"))]
            fusion_disabled: false,
        }
    }
}

impl QueryDbConfig {
    /// Returns a builder for fluent configuration.
    #[must_use]
    pub fn builder() -> QueryDbConfigBuilder {
        QueryDbConfigBuilder(Self::default())
    }

    /// Test-only: returns a copy of this config with planner fusion
    /// disabled (i.e., [`Self::fusion_disabled`] set to `true`).
    ///
    /// Gated behind `cfg(any(test, feature = "baseline"))` to match the
    /// gating of [`Self::fusion_disabled`] itself; production builds
    /// (release `sqry`, `sqry-mcp`, `sqry-lsp`, `sqryd`) never carry this
    /// symbol.
    ///
    /// Consumed by `sqry-db/tests/property/fusion_equivalence.rs`
    /// (`U_WS1_9_FUSION`, DESIGN §2.4). See the field-level docstring on
    /// [`Self::fusion_disabled`] for end-to-end semantics.
    #[cfg(any(test, feature = "baseline"))]
    #[must_use]
    pub fn disable_fusion(mut self) -> Self {
        self.fusion_disabled = true;
        self
    }

    /// Creates a config from environment variables, falling back to defaults.
    ///
    /// Recognized variables:
    /// - `SQRY_DB_SHARD_COUNT` — cache shard count (power of two)
    /// - `SQRY_DB_COMPACTION_FRAG` — fragmentation threshold (float)
    /// - `SQRY_DB_COMPACTION_DELTA` — delta ratio threshold (float)
    /// - `SQRY_DB_DERIVED_FILE` — persistence filename
    /// - `SQRY_DB_BG_COMPACTION` — `0` to disable background compaction
    /// - `SQRY_DB_MAX_ENTRY_SIZE_BYTES` — maximum serialized bytes per cached
    ///   entry; must parse as a positive `usize` (rejects `0` and
    ///   non-numeric values — falls back to default of 1 MiB)
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_env_impl(|key| std::env::var(key).ok())
    }

    /// Inner implementation of [`from_env`] that accepts an arbitrary env
    /// lookup function.  Separating the two lets unit tests inject a fake
    /// environment without touching the real process environment.
    fn from_env_impl(getter: impl Fn(&str) -> Option<String>) -> Self {
        let mut cfg = Self::default();

        if let Some(v) = getter("SQRY_DB_SHARD_COUNT")
            && let Ok(n) = v.parse::<usize>()
            && n.is_power_of_two()
            && n > 0
        {
            cfg.shard_count = n;
        }
        if let Some(v) = getter("SQRY_DB_COMPACTION_FRAG")
            && let Ok(f) = v.parse::<f64>()
            && (0.0..=1.0).contains(&f)
        {
            cfg.compaction_fragmentation_threshold = f;
        }
        if let Some(v) = getter("SQRY_DB_COMPACTION_DELTA")
            && let Ok(f) = v.parse::<f64>()
            && (0.0..=1.0).contains(&f)
        {
            cfg.compaction_delta_ratio_threshold = f;
        }
        if let Some(v) = getter("SQRY_DB_DERIVED_FILE")
            && !v.is_empty()
        {
            cfg.derived_persistence_filename = v;
        }
        if let Some(v) = getter("SQRY_DB_BG_COMPACTION") {
            cfg.enable_background_compaction = v != "0";
        }
        if let Some(v) = getter("SQRY_DB_MAX_ENTRY_SIZE_BYTES") {
            match v.parse::<usize>() {
                Ok(0) => {
                    log::warn!(
                        "SQRY_DB_MAX_ENTRY_SIZE_BYTES=0 is invalid (must be > 0); \
                         using default {}",
                        cfg.max_entry_size_bytes
                    );
                }
                Ok(n) => {
                    cfg.max_entry_size_bytes = n;
                }
                Err(_) => {
                    log::warn!(
                        "SQRY_DB_MAX_ENTRY_SIZE_BYTES={:?} could not be parsed as usize; \
                         using default {}",
                        v,
                        cfg.max_entry_size_bytes
                    );
                }
            }
        }

        cfg
    }
}

/// Builder for [`QueryDbConfig`].
pub struct QueryDbConfigBuilder(QueryDbConfig);

impl QueryDbConfigBuilder {
    /// Sets the number of cache shards. Must be a power of two.
    ///
    /// # Panics
    ///
    /// Panics if `count` is not a power of two or is zero.
    #[must_use]
    pub fn shard_count(mut self, count: usize) -> Self {
        assert!(
            count > 0 && count.is_power_of_two(),
            "shard_count must be a positive power of two"
        );
        self.0.shard_count = count;
        self
    }

    /// Sets the arena fragmentation threshold (0.0–1.0).
    #[must_use]
    pub fn compaction_fragmentation_threshold(mut self, threshold: f64) -> Self {
        self.0.compaction_fragmentation_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Sets the delta buffer ratio threshold (0.0–1.0).
    #[must_use]
    pub fn compaction_delta_ratio_threshold(mut self, threshold: f64) -> Self {
        self.0.compaction_delta_ratio_threshold = threshold.clamp(0.0, 1.0);
        self
    }

    /// Sets the derived persistence filename.
    #[must_use]
    pub fn derived_persistence_filename(mut self, filename: impl Into<String>) -> Self {
        self.0.derived_persistence_filename = filename.into();
        self
    }

    /// Enables or disables background compaction.
    #[must_use]
    pub fn enable_background_compaction(mut self, enable: bool) -> Self {
        self.0.enable_background_compaction = enable;
        self
    }

    /// Sets the maximum serialized size (in bytes) for a single cached entry.
    ///
    /// Entries that exceed this limit are evaluated normally but not stored in
    /// the cache. See [`QueryDbConfig::max_entry_size_bytes`] for full
    /// semantics.
    ///
    /// # Panics
    ///
    /// Panics if `bytes` is zero.
    #[must_use]
    pub fn max_entry_size_bytes(mut self, bytes: usize) -> Self {
        assert!(bytes > 0, "max_entry_size_bytes must be > 0");
        self.0.max_entry_size_bytes = bytes;
        self
    }

    /// Builds the config.
    #[must_use]
    pub fn build(self) -> QueryDbConfig {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Default value
    // ---------------------------------------------------------------------------

    #[test]
    fn max_entry_size_bytes_default_is_1_mib() {
        let cfg = QueryDbConfig::default();
        assert_eq!(cfg.max_entry_size_bytes, 1 << 20);
        assert_eq!(cfg.max_entry_size_bytes, 1_048_576);
    }

    // ---------------------------------------------------------------------------
    // from_env_impl — deterministic, no global env state
    // ---------------------------------------------------------------------------

    fn fake_env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn max_entry_size_bytes_from_env_parses_valid() {
        let cfg =
            QueryDbConfig::from_env_impl(fake_env(&[("SQRY_DB_MAX_ENTRY_SIZE_BYTES", "2097152")]));
        assert_eq!(cfg.max_entry_size_bytes, 2_097_152);
    }

    #[test]
    fn max_entry_size_bytes_from_env_rejects_zero() {
        // Zero is rejected; field should remain at its default (1 MiB).
        let cfg = QueryDbConfig::from_env_impl(fake_env(&[("SQRY_DB_MAX_ENTRY_SIZE_BYTES", "0")]));
        assert_eq!(cfg.max_entry_size_bytes, 1 << 20);
    }

    #[test]
    fn max_entry_size_bytes_from_env_rejects_unparseable() {
        // Non-numeric value is rejected; field should remain at its default.
        let cfg = QueryDbConfig::from_env_impl(fake_env(&[(
            "SQRY_DB_MAX_ENTRY_SIZE_BYTES",
            "not_a_number",
        )]));
        assert_eq!(cfg.max_entry_size_bytes, 1 << 20);
    }

    // ---------------------------------------------------------------------------
    // Builder
    // ---------------------------------------------------------------------------

    #[test]
    #[should_panic(expected = "max_entry_size_bytes must be > 0")]
    fn builder_rejects_zero_max_entry_size_bytes() {
        let _ = QueryDbConfig::builder().max_entry_size_bytes(0).build();
    }

    #[test]
    fn builder_accepts_nonzero_max_entry_size_bytes() {
        let cfg = QueryDbConfig::builder()
            .max_entry_size_bytes(512 * 1024) // 512 KiB
            .build();
        assert_eq!(cfg.max_entry_size_bytes, 524_288);
    }
}
