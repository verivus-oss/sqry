//! Security configuration for CD queries
//!
//! Provides configuration for query execution limits including:
//! - Timeout (30s NON-NEGOTIABLE ceiling per spec AC-8)
//! - Result cap (10k default)
//! - Memory limit (512MB default)
//! - Cost estimation limits

use std::time::Duration;

/// Security controls for CD queries
///
/// **TIMEOUT ALIGNMENT** (per Codex iter3 review):
/// Default timeout is 30s to match spec AC-8 (NON-NEGOTIABLE).
/// This is the hard ceiling - CLI can allow users to specify LOWER values only.
///
/// **PRIVATE FIELDS** (per Codex iter6 review):
/// All fields are private to prevent bypassing security limits.
/// Use constructors (`default()`, `interactive()`, `with_timeout()`) and
/// getters (`timeout()`, `result_cap()`, etc.) to access values.
/// The 30s ceiling cannot be bypassed by direct field assignment.
#[derive(Debug, Clone)]
pub struct QuerySecurityConfig {
    /// Maximum query execution time (PRIVATE - use getter)
    timeout: Duration,

    /// Maximum results to return (PRIVATE - use getter)
    result_cap: usize,

    /// Maximum memory for query processing in bytes (PRIVATE - use getter)
    memory_limit: usize,

    /// Enable query logging (PRIVATE - use getter)
    audit_enabled: bool,

    /// Pre-execution cost limit (PRIVATE - use getter)
    cost_limit: usize,
}

impl QuerySecurityConfig {
    /// The hard ceiling timeout (30s) - NON-NEGOTIABLE per spec AC-8
    pub const HARD_CEILING_TIMEOUT: Duration = Duration::from_secs(30);

    /// Default result cap (10,000)
    pub const DEFAULT_RESULT_CAP: usize = 10_000;

    /// Default memory limit (512 MB)
    pub const DEFAULT_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

    /// Default cost limit (1M estimated operations)
    pub const DEFAULT_COST_LIMIT: usize = 1_000_000;

    /// Create a config for interactive use with shorter timeout
    ///
    /// Users can request shorter timeouts for faster feedback.
    /// Uses 10s timeout instead of 30s for responsiveness.
    #[must_use]
    pub fn interactive() -> Self {
        Self {
            timeout: Duration::from_secs(10), // Shorter for responsiveness
            ..Default::default()
        }
    }

    /// Create a config with a custom timeout (capped at 30s)
    ///
    /// **IMPORTANT**: The timeout is capped at 30s regardless of input.
    /// This enforces the NON-NEGOTIABLE security requirement from spec AC-8.
    ///
    /// **Builder pattern**: Can be chained from other constructors:
    /// `QuerySecurityConfig::default().with_timeout(Duration::from_secs(5))`
    #[must_use]
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self {
            // Cap at 30s - the NON-NEGOTIABLE hard ceiling
            timeout: timeout.min(Self::HARD_CEILING_TIMEOUT),
            ..self
        }
    }

    /// Create a config with a custom result cap
    ///
    /// **Builder pattern**: Can be chained from other constructors:
    /// `QuerySecurityConfig::default().with_result_cap(100)`
    #[must_use]
    pub fn with_result_cap(self, cap: usize) -> Self {
        Self {
            result_cap: cap,
            ..self
        }
    }

    /// Create a config with a custom memory limit (bytes)
    ///
    /// **Builder pattern**: Can be chained from other constructors:
    /// `QuerySecurityConfig::default().with_memory_limit(1024 * 1024)` // 1MB
    #[must_use]
    pub fn with_memory_limit(self, limit: usize) -> Self {
        Self {
            memory_limit: limit,
            ..self
        }
    }

    /// Create a config with a custom cost limit
    ///
    /// **Builder pattern**: Can be chained from other constructors:
    /// `QuerySecurityConfig::default().with_cost_limit(500_000)`
    #[must_use]
    pub fn with_cost_limit(self, limit: usize) -> Self {
        Self {
            cost_limit: limit,
            ..self
        }
    }

    /// Create a config with audit logging disabled
    ///
    /// **Builder pattern**: Can be chained from other constructors:
    /// `QuerySecurityConfig::default().without_audit()`
    #[must_use]
    pub fn without_audit(self) -> Self {
        Self {
            audit_enabled: false,
            ..self
        }
    }

    // ======== GETTERS (per Codex iter6 review) ========
    // Fields are private; use these getters to access values.

    /// Get the timeout duration
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Get the result cap
    #[must_use]
    pub fn result_cap(&self) -> usize {
        self.result_cap
    }

    /// Get the memory limit (bytes)
    #[must_use]
    pub fn memory_limit(&self) -> usize {
        self.memory_limit
    }

    /// Check if audit logging is enabled
    #[must_use]
    pub fn audit_enabled(&self) -> bool {
        self.audit_enabled
    }

    /// Get the cost limit
    #[must_use]
    pub fn cost_limit(&self) -> usize {
        self.cost_limit
    }
}

impl Default for QuerySecurityConfig {
    fn default() -> Self {
        Self {
            // **30s DEFAULT** (per Codex iter3 review, aligns with spec AC-8)
            // This is the NON-NEGOTIABLE hard ceiling
            timeout: Self::HARD_CEILING_TIMEOUT,
            result_cap: Self::DEFAULT_RESULT_CAP,
            memory_limit: Self::DEFAULT_MEMORY_LIMIT,
            audit_enabled: true,
            cost_limit: Self::DEFAULT_COST_LIMIT,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = QuerySecurityConfig::default();
        assert_eq!(config.timeout(), Duration::from_secs(30));
        assert_eq!(config.result_cap(), 10_000);
        assert_eq!(config.memory_limit(), 512 * 1024 * 1024);
        assert!(config.audit_enabled());
        assert_eq!(config.cost_limit(), 1_000_000);
    }

    #[test]
    fn test_interactive_config() {
        let config = QuerySecurityConfig::interactive();
        assert_eq!(config.timeout(), Duration::from_secs(10));
        // Other values should be default
        assert_eq!(config.result_cap(), 10_000);
    }

    #[test]
    fn test_timeout_capped_at_30s() {
        // Requesting 60s should be capped to 30s
        let config = QuerySecurityConfig::default().with_timeout(Duration::from_secs(60));
        assert_eq!(config.timeout(), Duration::from_secs(30));
    }

    #[test]
    fn test_timeout_under_ceiling() {
        // Requesting 5s should be allowed
        let config = QuerySecurityConfig::default().with_timeout(Duration::from_secs(5));
        assert_eq!(config.timeout(), Duration::from_secs(5));
    }

    #[test]
    fn test_builder_chain() {
        let config = QuerySecurityConfig::default()
            .with_timeout(Duration::from_secs(15))
            .with_result_cap(500)
            .with_memory_limit(64 * 1024 * 1024)
            .without_audit();

        assert_eq!(config.timeout(), Duration::from_secs(15));
        assert_eq!(config.result_cap(), 500);
        assert_eq!(config.memory_limit(), 64 * 1024 * 1024);
        assert!(!config.audit_enabled());
    }

    #[test]
    fn test_hard_ceiling_constant() {
        assert_eq!(
            QuerySecurityConfig::HARD_CEILING_TIMEOUT,
            Duration::from_secs(30)
        );
    }
}
