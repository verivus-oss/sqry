//! Session configuration options.
//!
//! This module defines [`SessionConfig`], which controls how the session
//! caching layer behaves (cache sizing, idle eviction, etc.). Defaults are
//! tuned for local development workloads but can be overridden via
//! environment variables to support larger repositories or CI scenarios.

use std::time::Duration;

/// Configuration options for the session cache.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Maximum number of symbol indexes retained in-memory.
    pub max_cached_indexes: usize,
    /// How long an index can remain idle before eviction.
    pub idle_timeout: Duration,
    /// Whether filesystem watching is enabled for automatic invalidation.
    pub enable_file_watching: bool,
    /// Interval between background cleanup passes.
    pub cleanup_interval: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            max_cached_indexes: 5,
            idle_timeout: Duration::from_secs(60),
            enable_file_watching: true,
            cleanup_interval: Duration::from_secs(10),
        }
    }
}

impl SessionConfig {
    /// Create a configuration using environment overrides when present.
    ///
    /// Supported environment variables:
    ///
    /// * `SQRY_SESSION_CACHE_SIZE` – maximum cached indexes (usize)
    /// * `SQRY_SESSION_TIMEOUT` – idle timeout in seconds (u64)
    /// * `SQRY_SESSION_CLEANUP_INTERVAL` – cleanup interval in seconds (u64)
    /// * `SQRY_NO_SESSION` – disable session features when set to `1`
    #[must_use]
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Apply overrides using a custom environment lookup function (tests only).
    fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        let mut config = Self::default();

        if let Some(size) = lookup("SQRY_SESSION_CACHE_SIZE")
            && let Ok(parsed) = size.parse::<usize>()
        {
            config.max_cached_indexes = parsed.max(1);
        }

        if let Some(timeout) = lookup("SQRY_SESSION_TIMEOUT")
            && let Ok(secs) = timeout.parse::<u64>()
        {
            config.idle_timeout = Duration::from_secs(secs.max(1));
        }

        if let Some(interval) = lookup("SQRY_SESSION_CLEANUP_INTERVAL")
            && let Ok(secs) = interval.parse::<u64>()
        {
            config.cleanup_interval = Duration::from_secs(secs.max(1));
        }

        if let Some(val) = lookup("SQRY_NO_SESSION") {
            config.enable_file_watching = val != "1";
        }

        config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = SessionConfig::default();
        assert_eq!(cfg.max_cached_indexes, 5);
        assert_eq!(cfg.idle_timeout, Duration::from_secs(60));
        assert_eq!(cfg.cleanup_interval, Duration::from_secs(10));
        assert!(cfg.enable_file_watching);
    }

    #[test]
    fn env_overrides_apply() {
        let cfg = SessionConfig::from_lookup(|key| match key {
            "SQRY_SESSION_CACHE_SIZE" => Some("10".into()),
            "SQRY_SESSION_TIMEOUT" => Some("120".into()),
            "SQRY_SESSION_CLEANUP_INTERVAL" => Some("30".into()),
            "SQRY_NO_SESSION" => Some("1".into()),
            _ => None,
        });
        assert_eq!(cfg.max_cached_indexes, 10);
        assert_eq!(cfg.idle_timeout, Duration::from_secs(120));
        assert_eq!(cfg.cleanup_interval, Duration::from_secs(30));
        assert!(!cfg.enable_file_watching);
    }

    #[test]
    fn invalid_env_values_fall_back_to_defaults() {
        let cfg = SessionConfig::from_lookup(|key| match key {
            "SQRY_SESSION_CACHE_SIZE" => Some("not-a-number".into()),
            "SQRY_SESSION_TIMEOUT" => Some("zero".into()),
            "SQRY_SESSION_CLEANUP_INTERVAL" => Some("oops".into()),
            _ => None,
        });
        assert_eq!(
            cfg.max_cached_indexes,
            SessionConfig::default().max_cached_indexes
        );
        assert_eq!(cfg.idle_timeout, SessionConfig::default().idle_timeout);
        assert_eq!(
            cfg.cleanup_interval,
            SessionConfig::default().cleanup_interval
        );
    }
}
