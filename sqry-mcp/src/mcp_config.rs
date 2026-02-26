//! MCP server configuration.
//!
//! This module contains configuration for the MCP server, including
//! timeout and retry settings.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::env;

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    /// Timeout for MCP tool execution in milliseconds (default: 60000)
    ///
    /// Controls the maximum time allowed for a tool to execute before
    /// being cancelled. This prevents long-running operations from blocking
    /// the MCP server.
    ///
    /// # Validation
    /// - Minimum: 1000ms (1 second)
    /// - Maximum: 600000ms (10 minutes, recommended)
    /// - Hard cap: 3600000ms (1 hour, absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_TIMEOUT_MS` environment variable.
    pub timeout_ms: u64,

    /// Retry delay for deadline exceeded errors in milliseconds (default: 500)
    ///
    /// When a tool execution exceeds its deadline, this value is returned
    /// to the client as a suggested retry delay. The client can use this
    /// to implement exponential backoff or other retry strategies.
    ///
    /// # Validation
    /// - Minimum: 100ms
    /// - Maximum: 30000ms (30 seconds, recommended)
    /// - Hard cap: 60000ms (1 minute, absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_RETRY_DELAY_MS` environment variable.
    pub retry_delay_ms: u64,

    /// Cache capacity for `trace_path` results (default: 256)
    ///
    /// Controls how many `trace_path` query results are cached to improve
    /// performance for repeated queries. Larger values use more memory
    /// but reduce computation for frequently requested paths.
    ///
    /// # Validation
    /// - Minimum: 16 (basic caching)
    /// - Maximum: 1024 (recommended)
    /// - Hard cap: 4096 (absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_TRACE_CACHE_SIZE` environment variable.
    pub trace_cache_size: usize,

    /// Cache capacity for subgraph results (default: 128)
    ///
    /// Controls how many subgraph extraction results are cached. Subgraphs
    /// are typically larger than trace paths, so a smaller cache is used.
    ///
    /// # Validation
    /// - Minimum: 8 (basic caching)
    /// - Maximum: 512 (recommended)
    /// - Hard cap: 2048 (absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_SUBGRAPH_CACHE_SIZE` environment variable.
    pub subgraph_cache_size: usize,

    /// Maximum edges to scan for cross-language analysis (default: 50000)
    ///
    /// Performance guard to prevent excessive scanning when analyzing
    /// cross-language dependencies. Large codebases may have millions of
    /// edges, so this limit prevents timeouts and memory issues.
    ///
    /// # Validation
    /// - Minimum: 1000 (basic analysis)
    /// - Maximum: 500000 (recommended for large codebases)
    /// - Hard cap: 1000000 (absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_MAX_CROSS_LANG_EDGES` environment variable.
    pub max_cross_lang_edges: usize,

    /// Engine cache capacity (default: 5)
    ///
    /// Controls how many workspace engine instances are cached in memory.
    /// Each workspace gets its own Engine with loaded index. Larger values
    /// allow working with more repositories simultaneously without reloading.
    ///
    /// # Validation
    /// - Minimum: 1 (cache at least one workspace)
    /// - Maximum: 100 (recommended for memory management)
    /// - Hard cap: 1000 (absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_ENGINE_CACHE_CAPACITY` environment variable.
    #[serde(default = "default_engine_cache_capacity")]
    pub engine_cache_capacity: usize,

    /// Discovery cache capacity (default: 100)
    ///
    /// Controls how many workspace path resolution results are cached.
    /// This cache maps user-provided paths to canonical workspace roots,
    /// avoiding repeated filesystem traversal.
    ///
    /// # Validation
    /// - Minimum: 10 (basic caching)
    /// - Maximum: 1000 (recommended)
    /// - Hard cap: 10000 (absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_DISCOVERY_CACHE_CAPACITY` environment variable.
    #[serde(default = "default_discovery_cache_capacity")]
    pub discovery_cache_capacity: usize,

    /// Trace path cache capacity (default: 256)
    ///
    /// Controls how many `trace_path` query results are cached to improve
    /// performance for repeated queries. This is the primary cache capacity
    /// field; the legacy `trace_cache_size` field is retained for backward
    /// compatibility but this field takes precedence.
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_TRACE_PATH_CACHE_CAPACITY` environment variable.
    #[serde(default = "default_trace_path_cache_capacity")]
    pub trace_path_cache_capacity: usize,

    /// Subgraph cache capacity (default: 128)
    ///
    /// Controls how many subgraph extraction results are cached. This is the
    /// primary cache capacity field; the legacy `subgraph_cache_size` field
    /// is retained for backward compatibility but this field takes precedence.
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_SUBGRAPH_CACHE_CAPACITY` environment variable.
    #[serde(default = "default_subgraph_cache_capacity")]
    pub subgraph_cache_capacity: usize,

    /// Query cache TTL in seconds (default: 300)
    ///
    /// Time-to-live for cached query results (`trace_path`, subgraph).
    /// Expired entries are evicted on access. Lower values keep results
    /// fresher but increase computation. Higher values improve performance
    /// but may serve stale results.
    ///
    /// # Validation
    /// - Minimum: 10 seconds (avoid thrashing)
    /// - Maximum: 3600 seconds (1 hour, recommended)
    /// - Hard cap: 86400 seconds (24 hours, absolute maximum)
    ///
    /// # Environment Override
    /// Can be overridden with `SQRY_MCP_QUERY_CACHE_TTL_SECS` environment variable.
    #[serde(default = "default_query_cache_ttl")]
    pub query_cache_ttl_secs: u64,
}

fn default_engine_cache_capacity() -> usize {
    5
}

fn default_discovery_cache_capacity() -> usize {
    100
}

fn default_trace_path_cache_capacity() -> usize {
    256
}

fn default_subgraph_cache_capacity() -> usize {
    128
}

fn default_query_cache_ttl() -> u64 {
    300
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 60_000,
            retry_delay_ms: 500,
            trace_cache_size: 256,
            subgraph_cache_size: 128,
            max_cross_lang_edges: 50_000,
            engine_cache_capacity: default_engine_cache_capacity(),
            discovery_cache_capacity: default_discovery_cache_capacity(),
            trace_path_cache_capacity: default_trace_path_cache_capacity(),
            subgraph_cache_capacity: default_subgraph_cache_capacity(),
            query_cache_ttl_secs: default_query_cache_ttl(),
        }
    }
}

impl McpConfig {
    /// Minimum allowed timeout (1 second)
    const MIN_TIMEOUT_MS: u64 = 1000;

    /// Maximum recommended timeout (10 minutes)
    const MAX_TIMEOUT_MS: u64 = 600_000;

    /// Absolute hard cap for timeout (1 hour, security constraint)
    const ABSOLUTE_MAX_TIMEOUT_MS: u64 = 3_600_000;

    /// Minimum allowed retry delay (100ms)
    const MIN_RETRY_DELAY_MS: u64 = 100;

    /// Maximum recommended retry delay (30 seconds)
    const MAX_RETRY_DELAY_MS: u64 = 30_000;

    /// Absolute hard cap for retry delay (1 minute, security constraint)
    const ABSOLUTE_MAX_RETRY_DELAY_MS: u64 = 60_000;

    /// Minimum allowed trace cache size
    const MIN_TRACE_CACHE_SIZE: usize = 16;

    /// Maximum recommended trace cache size
    const MAX_TRACE_CACHE_SIZE: usize = 1024;

    /// Absolute hard cap for trace cache size
    const ABSOLUTE_MAX_TRACE_CACHE_SIZE: usize = 4096;

    /// Minimum allowed subgraph cache size
    const MIN_SUBGRAPH_CACHE_SIZE: usize = 8;

    /// Maximum recommended subgraph cache size
    const MAX_SUBGRAPH_CACHE_SIZE: usize = 512;

    /// Absolute hard cap for subgraph cache size
    const ABSOLUTE_MAX_SUBGRAPH_CACHE_SIZE: usize = 2048;

    /// Minimum allowed cross-language edges scan limit
    const MIN_CROSS_LANG_EDGES: usize = 1000;

    /// Maximum recommended cross-language edges scan limit
    const MAX_CROSS_LANG_EDGES: usize = 500_000;

    /// Absolute hard cap for cross-language edges scan limit
    const ABSOLUTE_MAX_CROSS_LANG_EDGES: usize = 1_000_000;

    /// Minimum allowed engine cache capacity
    #[allow(dead_code)] // Used for validation logic consistency
    const MIN_ENGINE_CACHE_CAPACITY: usize = 1;

    /// Maximum recommended engine cache capacity
    const MAX_ENGINE_CACHE_CAPACITY: usize = 100;

    /// Absolute hard cap for engine cache capacity
    const ABSOLUTE_MAX_ENGINE_CACHE_CAPACITY: usize = 1000;

    /// Minimum allowed discovery cache capacity
    const MIN_DISCOVERY_CACHE_CAPACITY: usize = 10;

    /// Maximum recommended discovery cache capacity
    const MAX_DISCOVERY_CACHE_CAPACITY: usize = 1000;

    /// Absolute hard cap for discovery cache capacity
    const ABSOLUTE_MAX_DISCOVERY_CACHE_CAPACITY: usize = 10_000;

    /// Minimum allowed trace path cache capacity (overrides `trace_cache_size`)
    const MIN_TRACE_PATH_CACHE_CAPACITY: usize = 16;

    /// Maximum recommended trace path cache capacity
    const MAX_TRACE_PATH_CACHE_CAPACITY: usize = 1024;

    /// Absolute hard cap for trace path cache capacity
    const ABSOLUTE_MAX_TRACE_PATH_CACHE_CAPACITY: usize = 4096;

    /// Minimum allowed subgraph cache capacity (overrides `subgraph_cache_size`)
    const MIN_SUBGRAPH_CACHE_CAPACITY: usize = 8;

    /// Maximum recommended subgraph cache capacity
    const MAX_SUBGRAPH_CACHE_CAPACITY: usize = 512;

    /// Absolute hard cap for subgraph cache capacity
    const ABSOLUTE_MAX_SUBGRAPH_CACHE_CAPACITY: usize = 2048;

    /// Minimum allowed query cache TTL
    const MIN_QUERY_CACHE_TTL_SECS: u64 = 10;

    /// Maximum recommended query cache TTL
    const MAX_QUERY_CACHE_TTL_SECS: u64 = 3600;

    /// Absolute hard cap for query cache TTL
    const ABSOLUTE_MAX_QUERY_CACHE_TTL_SECS: u64 = 86_400;

    /// Create a new MCP configuration with custom values
    ///
    /// # Errors
    ///
    /// Returns an error if validation fails (invalid cache sizes, timeouts, etc.)
    #[allow(dead_code)] // Used in tests or programmatic configuration setup.
    pub fn new(
        timeout_ms: u64,
        retry_delay_ms: u64,
        trace_cache_size: usize,
        subgraph_cache_size: usize,
        max_cross_lang_edges: usize,
    ) -> Result<Self> {
        let config = Self {
            timeout_ms,
            retry_delay_ms,
            trace_cache_size,
            subgraph_cache_size,
            max_cross_lang_edges,
            engine_cache_capacity: default_engine_cache_capacity(),
            discovery_cache_capacity: default_discovery_cache_capacity(),
            trace_path_cache_capacity: default_trace_path_cache_capacity(),
            subgraph_cache_capacity: default_subgraph_cache_capacity(),
            query_cache_ttl_secs: default_query_cache_ttl(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Load configuration with environment variable overrides
    ///
    /// # Errors
    ///
    /// Returns an error if environment variables contain invalid values or validation fails
    pub fn load_or_default() -> Result<Self> {
        let mut config = Self::default();

        // Apply environment variable overrides if present
        if let Ok(timeout_str) = env::var("SQRY_MCP_TIMEOUT_MS") {
            config.timeout_ms = Self::parse_env_var(&timeout_str, "SQRY_MCP_TIMEOUT_MS")?;
        }

        if let Ok(retry_str) = env::var("SQRY_MCP_RETRY_DELAY_MS") {
            config.retry_delay_ms = Self::parse_env_var(&retry_str, "SQRY_MCP_RETRY_DELAY_MS")?;
        }

        if let Ok(trace_str) = env::var("SQRY_MCP_TRACE_CACHE_SIZE") {
            config.trace_cache_size =
                Self::parse_env_var_usize(&trace_str, "SQRY_MCP_TRACE_CACHE_SIZE")?;
        }

        if let Ok(subgraph_str) = env::var("SQRY_MCP_SUBGRAPH_CACHE_SIZE") {
            config.subgraph_cache_size =
                Self::parse_env_var_usize(&subgraph_str, "SQRY_MCP_SUBGRAPH_CACHE_SIZE")?;
        }

        if let Ok(edges_str) = env::var("SQRY_MCP_MAX_CROSS_LANG_EDGES") {
            config.max_cross_lang_edges =
                Self::parse_env_var_usize(&edges_str, "SQRY_MCP_MAX_CROSS_LANG_EDGES")?;
        }

        if let Ok(engine_cache_str) = env::var("SQRY_MCP_ENGINE_CACHE_CAPACITY") {
            config.engine_cache_capacity =
                Self::parse_env_var_usize(&engine_cache_str, "SQRY_MCP_ENGINE_CACHE_CAPACITY")?;
        }

        if let Ok(discovery_cache_str) = env::var("SQRY_MCP_DISCOVERY_CACHE_CAPACITY") {
            config.discovery_cache_capacity = Self::parse_env_var_usize(
                &discovery_cache_str,
                "SQRY_MCP_DISCOVERY_CACHE_CAPACITY",
            )?;
        }

        if let Ok(trace_path_cache_str) = env::var("SQRY_MCP_TRACE_PATH_CACHE_CAPACITY") {
            config.trace_path_cache_capacity = Self::parse_env_var_usize(
                &trace_path_cache_str,
                "SQRY_MCP_TRACE_PATH_CACHE_CAPACITY",
            )?;
        }

        if let Ok(subgraph_cache_str) = env::var("SQRY_MCP_SUBGRAPH_CACHE_CAPACITY") {
            config.subgraph_cache_capacity =
                Self::parse_env_var_usize(&subgraph_cache_str, "SQRY_MCP_SUBGRAPH_CACHE_CAPACITY")?;
        }

        if let Ok(ttl_str) = env::var("SQRY_MCP_QUERY_CACHE_TTL_SECS") {
            config.query_cache_ttl_secs =
                Self::parse_env_var(&ttl_str, "SQRY_MCP_QUERY_CACHE_TTL_SECS")?;
        }

        config.validate()?;
        Ok(config)
    }

    /// Get effective timeout with validation
    ///
    /// This method enforces all validation constraints and returns the
    /// safe-to-use timeout value.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (unlimited not allowed)
    /// - Value is below minimum (< 1000ms)
    /// - Value exceeds hard cap (> 3600000ms)
    pub fn effective_timeout_ms(&self) -> Result<u64> {
        if self.timeout_ms == 0 {
            bail!("mcp.timeout_ms cannot be 0 (unlimited not allowed for safety)");
        }

        if self.timeout_ms < Self::MIN_TIMEOUT_MS {
            bail!(
                "mcp.timeout_ms {} is below minimum {}ms",
                self.timeout_ms,
                Self::MIN_TIMEOUT_MS
            );
        }

        if self.timeout_ms > Self::MAX_TIMEOUT_MS {
            tracing::warn!(
                "mcp.timeout_ms {} exceeds recommended maximum {}ms",
                self.timeout_ms,
                Self::MAX_TIMEOUT_MS
            );
        }

        if self.timeout_ms > Self::ABSOLUTE_MAX_TIMEOUT_MS {
            bail!(
                "mcp.timeout_ms {} exceeds absolute hard cap {}ms",
                self.timeout_ms,
                Self::ABSOLUTE_MAX_TIMEOUT_MS
            );
        }

        Ok(self.timeout_ms)
    }

    /// Get effective retry delay with validation
    ///
    /// This method enforces all validation constraints and returns the
    /// safe-to-use retry delay value.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (no delay not allowed)
    /// - Value is below minimum (< 100ms)
    /// - Value exceeds hard cap (> 60000ms)
    pub fn effective_retry_delay_ms(&self) -> Result<u64> {
        if self.retry_delay_ms == 0 {
            bail!("mcp.retry_delay_ms cannot be 0 (no delay not allowed for safety)");
        }

        if self.retry_delay_ms < Self::MIN_RETRY_DELAY_MS {
            bail!(
                "mcp.retry_delay_ms {} is below minimum {}ms",
                self.retry_delay_ms,
                Self::MIN_RETRY_DELAY_MS
            );
        }

        if self.retry_delay_ms > Self::MAX_RETRY_DELAY_MS {
            tracing::warn!(
                "mcp.retry_delay_ms {} exceeds recommended maximum {}ms",
                self.retry_delay_ms,
                Self::MAX_RETRY_DELAY_MS
            );
        }

        if self.retry_delay_ms > Self::ABSOLUTE_MAX_RETRY_DELAY_MS {
            bail!(
                "mcp.retry_delay_ms {} exceeds absolute hard cap {}ms",
                self.retry_delay_ms,
                Self::ABSOLUTE_MAX_RETRY_DELAY_MS
            );
        }

        Ok(self.retry_delay_ms)
    }

    /// Get effective trace cache size with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (caching disabled not allowed)
    /// - Value is below minimum (< 16)
    /// - Value exceeds hard cap (> 4096)
    pub fn effective_trace_cache_size(&self) -> Result<usize> {
        if self.trace_cache_size == 0 {
            bail!("mcp.trace_cache_size cannot be 0 (caching disabled not allowed)");
        }

        if self.trace_cache_size < Self::MIN_TRACE_CACHE_SIZE {
            bail!(
                "mcp.trace_cache_size {} is below minimum {}",
                self.trace_cache_size,
                Self::MIN_TRACE_CACHE_SIZE
            );
        }

        if self.trace_cache_size > Self::MAX_TRACE_CACHE_SIZE {
            tracing::warn!(
                "mcp.trace_cache_size {} exceeds recommended maximum {}",
                self.trace_cache_size,
                Self::MAX_TRACE_CACHE_SIZE
            );
        }

        if self.trace_cache_size > Self::ABSOLUTE_MAX_TRACE_CACHE_SIZE {
            bail!(
                "mcp.trace_cache_size {} exceeds absolute hard cap {}",
                self.trace_cache_size,
                Self::ABSOLUTE_MAX_TRACE_CACHE_SIZE
            );
        }

        Ok(self.trace_cache_size)
    }

    /// Get effective subgraph cache size with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (caching disabled not allowed)
    /// - Value is below minimum (< 8)
    /// - Value exceeds hard cap (> 2048)
    pub fn effective_subgraph_cache_size(&self) -> Result<usize> {
        if self.subgraph_cache_size == 0 {
            bail!("mcp.subgraph_cache_size cannot be 0 (caching disabled not allowed)");
        }

        if self.subgraph_cache_size < Self::MIN_SUBGRAPH_CACHE_SIZE {
            bail!(
                "mcp.subgraph_cache_size {} is below minimum {}",
                self.subgraph_cache_size,
                Self::MIN_SUBGRAPH_CACHE_SIZE
            );
        }

        if self.subgraph_cache_size > Self::MAX_SUBGRAPH_CACHE_SIZE {
            tracing::warn!(
                "mcp.subgraph_cache_size {} exceeds recommended maximum {}",
                self.subgraph_cache_size,
                Self::MAX_SUBGRAPH_CACHE_SIZE
            );
        }

        if self.subgraph_cache_size > Self::ABSOLUTE_MAX_SUBGRAPH_CACHE_SIZE {
            bail!(
                "mcp.subgraph_cache_size {} exceeds absolute hard cap {}",
                self.subgraph_cache_size,
                Self::ABSOLUTE_MAX_SUBGRAPH_CACHE_SIZE
            );
        }

        Ok(self.subgraph_cache_size)
    }

    /// Get effective max cross-language edges limit with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (analysis disabled not allowed)
    /// - Value is below minimum (< 1000)
    /// - Value exceeds hard cap (> 1000000)
    pub fn effective_max_cross_lang_edges(&self) -> Result<usize> {
        if self.max_cross_lang_edges == 0 {
            bail!("mcp.max_cross_lang_edges cannot be 0 (analysis disabled not allowed)");
        }

        if self.max_cross_lang_edges < Self::MIN_CROSS_LANG_EDGES {
            bail!(
                "mcp.max_cross_lang_edges {} is below minimum {}",
                self.max_cross_lang_edges,
                Self::MIN_CROSS_LANG_EDGES
            );
        }

        if self.max_cross_lang_edges > Self::MAX_CROSS_LANG_EDGES {
            tracing::warn!(
                "mcp.max_cross_lang_edges {} exceeds recommended maximum {}",
                self.max_cross_lang_edges,
                Self::MAX_CROSS_LANG_EDGES
            );
        }

        if self.max_cross_lang_edges > Self::ABSOLUTE_MAX_CROSS_LANG_EDGES {
            bail!(
                "mcp.max_cross_lang_edges {} exceeds absolute hard cap {}",
                self.max_cross_lang_edges,
                Self::ABSOLUTE_MAX_CROSS_LANG_EDGES
            );
        }

        Ok(self.max_cross_lang_edges)
    }

    /// Get effective engine cache capacity with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (caching disabled not allowed)
    /// - Value exceeds hard cap (> 1000)
    pub fn effective_engine_cache_capacity(&self) -> Result<usize> {
        if self.engine_cache_capacity == 0 {
            bail!("mcp.engine_cache_capacity cannot be 0 (at least 1 workspace required)");
        }

        if self.engine_cache_capacity > Self::MAX_ENGINE_CACHE_CAPACITY {
            tracing::warn!(
                "mcp.engine_cache_capacity {} exceeds recommended maximum {}",
                self.engine_cache_capacity,
                Self::MAX_ENGINE_CACHE_CAPACITY
            );
        }

        if self.engine_cache_capacity > Self::ABSOLUTE_MAX_ENGINE_CACHE_CAPACITY {
            bail!(
                "mcp.engine_cache_capacity {} exceeds absolute hard cap {}",
                self.engine_cache_capacity,
                Self::ABSOLUTE_MAX_ENGINE_CACHE_CAPACITY
            );
        }

        Ok(self.engine_cache_capacity)
    }

    /// Get effective discovery cache capacity with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (caching disabled not allowed)
    /// - Value is below minimum (< 10)
    /// - Value exceeds hard cap (> 10000)
    pub fn effective_discovery_cache_capacity(&self) -> Result<usize> {
        if self.discovery_cache_capacity == 0 {
            bail!("mcp.discovery_cache_capacity cannot be 0 (caching disabled not allowed)");
        }

        if self.discovery_cache_capacity < Self::MIN_DISCOVERY_CACHE_CAPACITY {
            bail!(
                "mcp.discovery_cache_capacity {} is below minimum {}",
                self.discovery_cache_capacity,
                Self::MIN_DISCOVERY_CACHE_CAPACITY
            );
        }

        if self.discovery_cache_capacity > Self::MAX_DISCOVERY_CACHE_CAPACITY {
            tracing::warn!(
                "mcp.discovery_cache_capacity {} exceeds recommended maximum {}",
                self.discovery_cache_capacity,
                Self::MAX_DISCOVERY_CACHE_CAPACITY
            );
        }

        if self.discovery_cache_capacity > Self::ABSOLUTE_MAX_DISCOVERY_CACHE_CAPACITY {
            bail!(
                "mcp.discovery_cache_capacity {} exceeds absolute hard cap {}",
                self.discovery_cache_capacity,
                Self::ABSOLUTE_MAX_DISCOVERY_CACHE_CAPACITY
            );
        }

        Ok(self.discovery_cache_capacity)
    }

    /// Get effective trace path cache capacity with validation
    ///
    /// This takes precedence over `trace_cache_size`.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (caching disabled not allowed)
    /// - Value is below minimum (< 16)
    /// - Value exceeds hard cap (> 4096)
    pub fn effective_trace_path_cache_capacity(&self) -> Result<usize> {
        if self.trace_path_cache_capacity == 0 {
            bail!("mcp.trace_path_cache_capacity cannot be 0 (caching disabled not allowed)");
        }

        if self.trace_path_cache_capacity < Self::MIN_TRACE_PATH_CACHE_CAPACITY {
            bail!(
                "mcp.trace_path_cache_capacity {} is below minimum {}",
                self.trace_path_cache_capacity,
                Self::MIN_TRACE_PATH_CACHE_CAPACITY
            );
        }

        if self.trace_path_cache_capacity > Self::MAX_TRACE_PATH_CACHE_CAPACITY {
            tracing::warn!(
                "mcp.trace_path_cache_capacity {} exceeds recommended maximum {}",
                self.trace_path_cache_capacity,
                Self::MAX_TRACE_PATH_CACHE_CAPACITY
            );
        }

        if self.trace_path_cache_capacity > Self::ABSOLUTE_MAX_TRACE_PATH_CACHE_CAPACITY {
            bail!(
                "mcp.trace_path_cache_capacity {} exceeds absolute hard cap {}",
                self.trace_path_cache_capacity,
                Self::ABSOLUTE_MAX_TRACE_PATH_CACHE_CAPACITY
            );
        }

        Ok(self.trace_path_cache_capacity)
    }

    /// Get effective subgraph cache capacity with validation
    ///
    /// This takes precedence over `subgraph_cache_size`.
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (caching disabled not allowed)
    /// - Value is below minimum (< 8)
    /// - Value exceeds hard cap (> 2048)
    pub fn effective_subgraph_cache_capacity(&self) -> Result<usize> {
        if self.subgraph_cache_capacity == 0 {
            bail!("mcp.subgraph_cache_capacity cannot be 0 (caching disabled not allowed)");
        }

        if self.subgraph_cache_capacity < Self::MIN_SUBGRAPH_CACHE_CAPACITY {
            bail!(
                "mcp.subgraph_cache_capacity {} is below minimum {}",
                self.subgraph_cache_capacity,
                Self::MIN_SUBGRAPH_CACHE_CAPACITY
            );
        }

        if self.subgraph_cache_capacity > Self::MAX_SUBGRAPH_CACHE_CAPACITY {
            tracing::warn!(
                "mcp.subgraph_cache_capacity {} exceeds recommended maximum {}",
                self.subgraph_cache_capacity,
                Self::MAX_SUBGRAPH_CACHE_CAPACITY
            );
        }

        if self.subgraph_cache_capacity > Self::ABSOLUTE_MAX_SUBGRAPH_CACHE_CAPACITY {
            bail!(
                "mcp.subgraph_cache_capacity {} exceeds absolute hard cap {}",
                self.subgraph_cache_capacity,
                Self::ABSOLUTE_MAX_SUBGRAPH_CACHE_CAPACITY
            );
        }

        Ok(self.subgraph_cache_capacity)
    }

    /// Get effective query cache TTL with validation
    ///
    /// # Errors
    /// Returns an error if:
    /// - Value is 0 (no TTL not allowed)
    /// - Value is below minimum (< 10 seconds)
    /// - Value exceeds hard cap (> 86400 seconds)
    pub fn effective_query_cache_ttl_secs(&self) -> Result<u64> {
        if self.query_cache_ttl_secs == 0 {
            bail!("mcp.query_cache_ttl_secs cannot be 0 (TTL required for freshness)");
        }

        if self.query_cache_ttl_secs < Self::MIN_QUERY_CACHE_TTL_SECS {
            bail!(
                "mcp.query_cache_ttl_secs {} is below minimum {}",
                self.query_cache_ttl_secs,
                Self::MIN_QUERY_CACHE_TTL_SECS
            );
        }

        if self.query_cache_ttl_secs > Self::MAX_QUERY_CACHE_TTL_SECS {
            tracing::warn!(
                "mcp.query_cache_ttl_secs {} exceeds recommended maximum {}",
                self.query_cache_ttl_secs,
                Self::MAX_QUERY_CACHE_TTL_SECS
            );
        }

        if self.query_cache_ttl_secs > Self::ABSOLUTE_MAX_QUERY_CACHE_TTL_SECS {
            bail!(
                "mcp.query_cache_ttl_secs {} exceeds absolute hard cap {}",
                self.query_cache_ttl_secs,
                Self::ABSOLUTE_MAX_QUERY_CACHE_TTL_SECS
            );
        }

        Ok(self.query_cache_ttl_secs)
    }

    /// Validate the configuration
    fn validate(&self) -> Result<()> {
        // Validation happens in effective_* methods
        self.effective_timeout_ms()?;
        self.effective_retry_delay_ms()?;
        self.effective_trace_cache_size()?;
        self.effective_subgraph_cache_size()?;
        self.effective_max_cross_lang_edges()?;
        self.effective_engine_cache_capacity()?;
        self.effective_discovery_cache_capacity()?;
        self.effective_trace_path_cache_capacity()?;
        self.effective_subgraph_cache_capacity()?;
        self.effective_query_cache_ttl_secs()?;
        Ok(())
    }

    /// Parse environment variable with strict error handling
    ///
    /// This method implements the fail-fast parse policy:
    /// - Parse errors result in immediate failure with clear message
    /// - Out-of-range values result in immediate failure
    /// - NO silent fallbacks to default values
    fn parse_env_var(value: &str, var_name: &str) -> Result<u64> {
        match value.parse::<u64>() {
            Ok(parsed) => Ok(parsed),
            Err(_) => bail!("Invalid value for {var_name}: '{value}'. Expected u64 milliseconds"),
        }
    }

    /// Parse usize environment variable with strict error handling
    ///
    /// This method implements the fail-fast parse policy for usize values:
    /// - Parse errors result in immediate failure with clear message
    /// - Out-of-range values result in immediate failure
    /// - NO silent fallbacks to default values
    fn parse_env_var_usize(value: &str, var_name: &str) -> Result<usize> {
        match value.parse::<usize>() {
            Ok(parsed) => Ok(parsed),
            Err(_) => bail!("Invalid value for {var_name}: '{value}'. Expected usize"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = McpConfig::default();
        assert_eq!(config.timeout_ms, 60_000);
        assert_eq!(config.retry_delay_ms, 500);
        assert_eq!(config.trace_cache_size, 256);
        assert_eq!(config.subgraph_cache_size, 128);
        assert_eq!(config.max_cross_lang_edges, 50_000);
        assert!(config.effective_timeout_ms().is_ok());
        assert!(config.effective_retry_delay_ms().is_ok());
        assert!(config.effective_trace_cache_size().is_ok());
        assert!(config.effective_subgraph_cache_size().is_ok());
        assert!(config.effective_max_cross_lang_edges().is_ok());
    }

    #[test]
    fn test_new_with_valid_values() {
        let config = McpConfig::new(5000, 1000, 512, 256, 100_000).unwrap();
        assert_eq!(config.effective_timeout_ms().unwrap(), 5000);
        assert_eq!(config.effective_retry_delay_ms().unwrap(), 1000);
        assert_eq!(config.effective_trace_cache_size().unwrap(), 512);
        assert_eq!(config.effective_subgraph_cache_size().unwrap(), 256);
        assert_eq!(config.effective_max_cross_lang_edges().unwrap(), 100_000);
    }

    // --- Table-driven boundary tests for McpConfig::new parameters ---

    struct NewParamCase {
        name: &'static str,
        timeout: u64,
        retry: u64,
        trace: usize,
        subgraph: usize,
        cross_lang: usize,
        expect_ok: bool,
        error_contains: &'static str,
    }

    #[test]
    fn test_new_boundary_cases() {
        let cases = [
            // timeout_ms boundaries
            NewParamCase {
                name: "timeout_zero",
                timeout: 0,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "cannot be 0",
            },
            NewParamCase {
                name: "timeout_below_min",
                timeout: 500,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "below minimum 1000ms",
            },
            NewParamCase {
                name: "timeout_at_min",
                timeout: 1000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "timeout_at_cap",
                timeout: 3_600_000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "timeout_above_cap",
                timeout: 3_600_001,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "exceeds absolute hard cap",
            },
            // retry_delay_ms boundaries
            NewParamCase {
                name: "retry_zero",
                timeout: 30_000,
                retry: 0,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "cannot be 0",
            },
            NewParamCase {
                name: "retry_below_min",
                timeout: 30_000,
                retry: 50,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "below minimum 100ms",
            },
            NewParamCase {
                name: "retry_at_min",
                timeout: 30_000,
                retry: 100,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "retry_at_cap",
                timeout: 30_000,
                retry: 60_000,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "retry_above_cap",
                timeout: 30_000,
                retry: 60_001,
                trace: 256,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "exceeds absolute hard cap",
            },
            // trace_cache_size boundaries
            NewParamCase {
                name: "trace_zero",
                timeout: 30_000,
                retry: 500,
                trace: 0,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "cannot be 0",
            },
            NewParamCase {
                name: "trace_below_min",
                timeout: 30_000,
                retry: 500,
                trace: 8,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "below minimum 16",
            },
            NewParamCase {
                name: "trace_at_min",
                timeout: 30_000,
                retry: 500,
                trace: 16,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "trace_at_cap",
                timeout: 30_000,
                retry: 500,
                trace: 4096,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "trace_above_cap",
                timeout: 30_000,
                retry: 500,
                trace: 4097,
                subgraph: 128,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "exceeds absolute hard cap",
            },
            // subgraph_cache_size boundaries
            NewParamCase {
                name: "subgraph_zero",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 0,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "cannot be 0",
            },
            NewParamCase {
                name: "subgraph_below_min",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 4,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "below minimum 8",
            },
            NewParamCase {
                name: "subgraph_at_min",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 8,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "subgraph_at_cap",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 2048,
                cross_lang: 50_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "subgraph_above_cap",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 2049,
                cross_lang: 50_000,
                expect_ok: false,
                error_contains: "exceeds absolute hard cap",
            },
            // max_cross_lang_edges boundaries
            NewParamCase {
                name: "cross_lang_zero",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 0,
                expect_ok: false,
                error_contains: "cannot be 0",
            },
            NewParamCase {
                name: "cross_lang_below_min",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 500,
                expect_ok: false,
                error_contains: "below minimum 1000",
            },
            NewParamCase {
                name: "cross_lang_at_min",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 1000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "cross_lang_at_cap",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 1_000_000,
                expect_ok: true,
                error_contains: "",
            },
            NewParamCase {
                name: "cross_lang_above_cap",
                timeout: 30_000,
                retry: 500,
                trace: 256,
                subgraph: 128,
                cross_lang: 1_000_001,
                expect_ok: false,
                error_contains: "exceeds absolute hard cap",
            },
        ];

        for case in &cases {
            let result = McpConfig::new(
                case.timeout,
                case.retry,
                case.trace,
                case.subgraph,
                case.cross_lang,
            );
            if case.expect_ok {
                assert!(
                    result.is_ok(),
                    "case '{}' should succeed but failed: {:?}",
                    case.name,
                    result.unwrap_err()
                );
            } else {
                assert!(
                    result.is_err(),
                    "case '{}' should fail but succeeded",
                    case.name
                );
                let err = result.unwrap_err().to_string();
                assert!(
                    err.contains(case.error_contains),
                    "case '{}': expected error containing '{}', got '{}'",
                    case.name,
                    case.error_contains,
                    err
                );
            }
        }
    }

    // --- Table-driven boundary tests for cache capacity fields ---

    struct CacheCapacityCase {
        name: &'static str,
        field: &'static str,
        value: usize,
        expect_ok: bool,
        expected_value: usize,
        error_contains: &'static str,
    }

    fn apply_and_check(case: &CacheCapacityCase) {
        let mut config = McpConfig::default();
        let result: Result<usize> = match case.field {
            "engine_cache_capacity" => {
                config.engine_cache_capacity = case.value;
                config.effective_engine_cache_capacity()
            }
            "discovery_cache_capacity" => {
                config.discovery_cache_capacity = case.value;
                config.effective_discovery_cache_capacity()
            }
            "trace_path_cache_capacity" => {
                config.trace_path_cache_capacity = case.value;
                config.effective_trace_path_cache_capacity()
            }
            "subgraph_cache_capacity" => {
                config.subgraph_cache_capacity = case.value;
                config.effective_subgraph_cache_capacity()
            }
            "query_cache_ttl_secs" => {
                config.query_cache_ttl_secs = case.value as u64;
                config.effective_query_cache_ttl_secs().map(|v| v as usize)
            }
            _ => panic!("Unknown field: {}", case.field),
        };

        if case.expect_ok {
            assert!(
                result.is_ok(),
                "case '{}' should succeed but failed: {:?}",
                case.name,
                result.unwrap_err()
            );
            assert_eq!(
                result.unwrap(),
                case.expected_value,
                "case '{}' value mismatch",
                case.name
            );
        } else {
            assert!(
                result.is_err(),
                "case '{}' should fail but succeeded",
                case.name
            );
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains(case.error_contains),
                "case '{}': expected error containing '{}', got '{}'",
                case.name,
                case.error_contains,
                err
            );
        }
    }

    #[test]
    fn test_cache_capacity_boundary_cases() {
        let cases = [
            // engine_cache_capacity: default=5, min=1 (no below_min distinct from zero), cap=1000
            CacheCapacityCase {
                name: "engine_default",
                field: "engine_cache_capacity",
                value: 5,
                expect_ok: true,
                expected_value: 5,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "engine_zero",
                field: "engine_cache_capacity",
                value: 0,
                expect_ok: false,
                expected_value: 0,
                error_contains: "cannot be 0",
            },
            CacheCapacityCase {
                name: "engine_at_cap",
                field: "engine_cache_capacity",
                value: 1000,
                expect_ok: true,
                expected_value: 1000,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "engine_above_cap",
                field: "engine_cache_capacity",
                value: 1001,
                expect_ok: false,
                expected_value: 0,
                error_contains: "exceeds absolute hard cap",
            },
            // discovery_cache_capacity: default=100, min=10, cap=10_000
            CacheCapacityCase {
                name: "discovery_default",
                field: "discovery_cache_capacity",
                value: 100,
                expect_ok: true,
                expected_value: 100,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "discovery_zero",
                field: "discovery_cache_capacity",
                value: 0,
                expect_ok: false,
                expected_value: 0,
                error_contains: "cannot be 0",
            },
            CacheCapacityCase {
                name: "discovery_below_min",
                field: "discovery_cache_capacity",
                value: 5,
                expect_ok: false,
                expected_value: 0,
                error_contains: "below minimum 10",
            },
            CacheCapacityCase {
                name: "discovery_at_cap",
                field: "discovery_cache_capacity",
                value: 10_000,
                expect_ok: true,
                expected_value: 10_000,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "discovery_above_cap",
                field: "discovery_cache_capacity",
                value: 10_001,
                expect_ok: false,
                expected_value: 0,
                error_contains: "exceeds absolute hard cap",
            },
            // trace_path_cache_capacity: default=256, min=16, cap=4096
            CacheCapacityCase {
                name: "trace_path_default",
                field: "trace_path_cache_capacity",
                value: 256,
                expect_ok: true,
                expected_value: 256,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "trace_path_zero",
                field: "trace_path_cache_capacity",
                value: 0,
                expect_ok: false,
                expected_value: 0,
                error_contains: "cannot be 0",
            },
            CacheCapacityCase {
                name: "trace_path_below_min",
                field: "trace_path_cache_capacity",
                value: 8,
                expect_ok: false,
                expected_value: 0,
                error_contains: "below minimum 16",
            },
            CacheCapacityCase {
                name: "trace_path_at_cap",
                field: "trace_path_cache_capacity",
                value: 4096,
                expect_ok: true,
                expected_value: 4096,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "trace_path_above_cap",
                field: "trace_path_cache_capacity",
                value: 4097,
                expect_ok: false,
                expected_value: 0,
                error_contains: "exceeds absolute hard cap",
            },
            // subgraph_cache_capacity: default=128, min=8, cap=2048
            CacheCapacityCase {
                name: "subgraph_cap_default",
                field: "subgraph_cache_capacity",
                value: 128,
                expect_ok: true,
                expected_value: 128,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "subgraph_cap_zero",
                field: "subgraph_cache_capacity",
                value: 0,
                expect_ok: false,
                expected_value: 0,
                error_contains: "cannot be 0",
            },
            CacheCapacityCase {
                name: "subgraph_cap_below_min",
                field: "subgraph_cache_capacity",
                value: 4,
                expect_ok: false,
                expected_value: 0,
                error_contains: "below minimum 8",
            },
            CacheCapacityCase {
                name: "subgraph_cap_at_cap",
                field: "subgraph_cache_capacity",
                value: 2048,
                expect_ok: true,
                expected_value: 2048,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "subgraph_cap_above_cap",
                field: "subgraph_cache_capacity",
                value: 2049,
                expect_ok: false,
                expected_value: 0,
                error_contains: "exceeds absolute hard cap",
            },
            // query_cache_ttl_secs: default=300, min=10, cap=86_400
            CacheCapacityCase {
                name: "ttl_default",
                field: "query_cache_ttl_secs",
                value: 300,
                expect_ok: true,
                expected_value: 300,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "ttl_zero",
                field: "query_cache_ttl_secs",
                value: 0,
                expect_ok: false,
                expected_value: 0,
                error_contains: "cannot be 0",
            },
            CacheCapacityCase {
                name: "ttl_below_min",
                field: "query_cache_ttl_secs",
                value: 5,
                expect_ok: false,
                expected_value: 0,
                error_contains: "below minimum 10",
            },
            CacheCapacityCase {
                name: "ttl_at_cap",
                field: "query_cache_ttl_secs",
                value: 86_400,
                expect_ok: true,
                expected_value: 86_400,
                error_contains: "",
            },
            CacheCapacityCase {
                name: "ttl_above_cap",
                field: "query_cache_ttl_secs",
                value: 86_401,
                expect_ok: false,
                expected_value: 0,
                error_contains: "exceeds absolute hard cap",
            },
        ];

        for case in &cases {
            apply_and_check(case);
        }
    }

    // --- Non-repetitive tests kept individually ---

    #[test]
    fn test_parse_env_var_valid() {
        let result = McpConfig::parse_env_var("5000", "TEST_VAR");
        assert_eq!(result.unwrap(), 5000);
    }

    #[test]
    fn test_parse_env_var_invalid() {
        let result = McpConfig::parse_env_var("abc", "TEST_VAR");
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Invalid value for TEST_VAR")
        );
    }

    #[test]
    fn test_parse_env_var_negative() {
        let result = McpConfig::parse_env_var("-1000", "TEST_VAR");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid value"));
    }

    #[test]
    fn test_effective_timeout_warns_at_max() {
        let config = McpConfig {
            timeout_ms: 600_000,
            retry_delay_ms: 500,
            trace_cache_size: 256,
            subgraph_cache_size: 128,
            max_cross_lang_edges: 50_000,
            engine_cache_capacity: default_engine_cache_capacity(),
            discovery_cache_capacity: default_discovery_cache_capacity(),
            trace_path_cache_capacity: default_trace_path_cache_capacity(),
            subgraph_cache_capacity: default_subgraph_cache_capacity(),
            query_cache_ttl_secs: default_query_cache_ttl(),
        };
        assert_eq!(config.effective_timeout_ms().unwrap(), 600_000);
    }

    #[test]
    fn test_effective_retry_delay_warns_at_max() {
        let config = McpConfig {
            timeout_ms: 30_000,
            retry_delay_ms: 30_000,
            trace_cache_size: 256,
            subgraph_cache_size: 128,
            max_cross_lang_edges: 50_000,
            engine_cache_capacity: default_engine_cache_capacity(),
            discovery_cache_capacity: default_discovery_cache_capacity(),
            trace_path_cache_capacity: default_trace_path_cache_capacity(),
            subgraph_cache_capacity: default_subgraph_cache_capacity(),
            query_cache_ttl_secs: default_query_cache_ttl(),
        };
        assert_eq!(config.effective_retry_delay_ms().unwrap(), 30_000);
    }

    #[test]
    fn test_config_validation() {
        let config = McpConfig::default();
        assert!(config.validate().is_ok());
    }
}
