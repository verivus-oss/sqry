//! Phase 3C DB21 Proof Point 4 — Config cleanup.
//!
//! From the spec (`docs/superpowers/specs/2026-04-12-derived-analysis-db-query-
//! planner-design.md`, "Proof 4: Config cleanup"):
//!
//! > Assert old cache config fields (`trace_path_cache_capacity`,
//! > `subgraph_cache_capacity`, `query_cache_ttl_secs`) are removed from
//! > `McpConfig`. Assert `init_trace_path_cache` and `init_subgraph_cache`
//! > initialization calls removed from `sqry-mcp/src/main.rs`. Assert
//! > `sqry-db` has its own config surface (shard count, compaction
//! > threshold, derived.sqry path).
//!
//! # Scope adjustment vs. the literal spec
//!
//! DB17 follow-up + DB19 confirmation retained
//! `sqry-mcp/src/execution/graph_cache.rs` (see the payload-vs-predicate
//! cache separation documented at the top of that module). The payload
//! cache is NOT a substitute for sqry-db's predicate cache; it is an
//! orthogonal response-DTO LRU. Its `init_trace_path_cache` and
//! `init_subgraph_cache` entrypoints are therefore retained — main.rs
//! still calls them.
//!
//! Consequently the literal spec assertion "init calls removed from
//! main.rs" is adjusted to: the calls still exist, but they are now
//! sized from the **retained** `trace_cache_size` / `subgraph_cache_size`
//! McpConfig fields and use the hardcoded TTL constant in
//! `execution::graph_cache::CACHE_TTL_SECS`. The three duplicate
//! `_capacity` / `_ttl_secs` knobs the spec targeted for deletion ARE
//! removed (both fields and environment overrides). Proof 4 tests lock
//! both the positive (new wiring) and the negative (retired fields)
//! invariants.

use std::env;

use sqry_db::QueryDbConfig;
use sqry_mcp::mcp_config::McpConfig;

/// Proof 4a — The retained `trace_cache_size` / `subgraph_cache_size`
/// McpConfig fields remain functional and validated. They are what
/// `sqry-mcp/src/main.rs` uses to size the payload caches after the
/// cleanup.
#[test]
fn proof4_retained_size_fields_remain_validated() {
    let config = McpConfig::default();
    assert_eq!(config.trace_cache_size, 256);
    assert_eq!(config.subgraph_cache_size, 128);
    assert!(config.effective_trace_cache_size().is_ok());
    assert!(config.effective_subgraph_cache_size().is_ok());

    // Hard cap assertions — the values formerly on the removed *_capacity
    // fields are now enforced by the retained *_size fields.
    let over = McpConfig {
        trace_cache_size: 4097,
        ..Default::default()
    };
    assert!(over.effective_trace_cache_size().is_err());

    let over_sub = McpConfig {
        subgraph_cache_size: 2049,
        ..Default::default()
    };
    assert!(over_sub.effective_subgraph_cache_size().is_err());
}

/// Proof 4b — The retired environment overrides are no longer honored.
/// Setting `SQRY_MCP_TRACE_PATH_CACHE_CAPACITY`,
/// `SQRY_MCP_SUBGRAPH_CACHE_CAPACITY`, or `SQRY_MCP_QUERY_CACHE_TTL_SECS`
/// must NOT influence any McpConfig field. We set clearly-invalid values
/// for all three and then assert that `load_or_default()` still returns
/// a valid config whose defaults are intact — i.e., the old code path
/// (which would have errored on validation for these values) is gone.
#[test]
fn proof4_retired_env_overrides_are_no_ops() {
    // SAFETY: the only writers of these env vars in the process are this
    // test block. Rust stdlib's `env::set_var` is documented as unsafe
    // because another thread could observe a torn read; this test is
    // single-threaded and deterministic. No other tests read these env
    // vars.
    // We use the legacy `set_var` / `remove_var`; if the compiler warns
    // about unsafety, wrap in an `unsafe {}` block on newer toolchains.
    unsafe {
        env::set_var("SQRY_MCP_TRACE_PATH_CACHE_CAPACITY", "99999");
        env::set_var("SQRY_MCP_SUBGRAPH_CACHE_CAPACITY", "99999");
        env::set_var("SQRY_MCP_QUERY_CACHE_TTL_SECS", "99999");
    }

    let config = McpConfig::load_or_default().expect(
        "load_or_default must succeed even with invalid values on the \
         retired env overrides — those env vars are now no-ops",
    );

    // The defaults are preserved — the retired env vars did NOT bleed
    // into any field.
    assert_eq!(config.trace_cache_size, 256);
    assert_eq!(config.subgraph_cache_size, 128);

    unsafe {
        env::remove_var("SQRY_MCP_TRACE_PATH_CACHE_CAPACITY");
        env::remove_var("SQRY_MCP_SUBGRAPH_CACHE_CAPACITY");
        env::remove_var("SQRY_MCP_QUERY_CACHE_TTL_SECS");
    }
}

/// Proof 4c — `sqry-db` has its own config surface with the shard count,
/// compaction thresholds, and derived persistence filename the spec
/// calls out. This is the "sqry-db has its own config" half of the
/// Proof 4 assertion.
#[test]
fn proof4_sqry_db_config_surface_is_complete() {
    let cfg = QueryDbConfig::default();

    // Shard count — power of two, default 64.
    assert_eq!(cfg.shard_count, 64);
    assert!(cfg.shard_count.is_power_of_two());

    // Compaction thresholds — both in [0.0, 1.0] and documented in the
    // QueryDbConfig type docs.
    assert!(cfg.compaction_fragmentation_threshold > 0.0);
    assert!(cfg.compaction_fragmentation_threshold <= 1.0);
    assert!(cfg.compaction_delta_ratio_threshold > 0.0);
    assert!(cfg.compaction_delta_ratio_threshold <= 1.0);

    // Derived persistence filename — default points at derived.sqry per
    // spec's "companion derived.sqry" language.
    assert_eq!(cfg.derived_persistence_filename, "derived.sqry");

    // Background compaction is enabled by default (async).
    assert!(cfg.enable_background_compaction);

    // Builder surface exists and round-trips through custom settings.
    let custom = QueryDbConfig::builder()
        .shard_count(128)
        .compaction_fragmentation_threshold(0.5)
        .compaction_delta_ratio_threshold(0.25)
        .derived_persistence_filename("custom-derived.sqry")
        .build();
    assert_eq!(custom.shard_count, 128);
    assert_eq!(custom.compaction_fragmentation_threshold, 0.5);
    assert_eq!(custom.compaction_delta_ratio_threshold, 0.25);
    assert_eq!(custom.derived_persistence_filename, "custom-derived.sqry");
}

/// Proof 4d — Compile-time guards that fail the build if a future
/// refactor accidentally re-introduces the retired fields or accessors.
///
/// Rust has no negative bounds ("this field must not exist"), so this
/// test instead pins the SHAPE of `McpConfig::default()`'s `Debug`
/// output. If any of the retired fields come back, the Debug output
/// will expand and this assertion will flag the regression.
#[test]
fn proof4_mcp_config_debug_shape_does_not_include_retired_fields() {
    let rendered = format!("{:?}", McpConfig::default());
    assert!(
        !rendered.contains("trace_path_cache_capacity"),
        "Retired field `trace_path_cache_capacity` reappeared in \
         McpConfig::default() Debug output: {rendered}"
    );
    assert!(
        !rendered.contains("subgraph_cache_capacity"),
        "Retired field `subgraph_cache_capacity` reappeared in \
         McpConfig::default() Debug output: {rendered}"
    );
    assert!(
        !rendered.contains("query_cache_ttl_secs"),
        "Retired field `query_cache_ttl_secs` reappeared in \
         McpConfig::default() Debug output: {rendered}"
    );
}
