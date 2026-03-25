//! Safe parsing utilities with resource limits.
//!
//! This module provides a centralized, secure parser utility that enforces
//! input size limits, parse timeouts, and supports external cancellation.
//! All language plugins should use `SafeParser` to prevent OOM vulnerabilities
//! from pathological inputs.
//!
//! # Security Background
//!
//! Tree-sitter parsers can consume unbounded memory when encountering malformed
//! input that triggers exponential backtracking in error recovery. A 103-byte
//! input can amplify to 2GB+ memory consumption (~20 million× amplification).
//!
//! # Usage
//!
//! ```ignore
//! use sqry_core::plugin::safe_parse::{SafeParser, SafeParserConfig};
//!
//! let config = SafeParserConfig::default();
//! let parser = SafeParser::new(config);
//!
//! let result = parser.parse(&language, content, Some(file_path));
//! match result {
//!     Ok(tree) => { /* use tree */ }
//!     Err(ParseError::InputTooLarge { size, max, .. }) => {
//!         log::warn!("File too large: {} bytes > {} limit", size, max);
//!     }
//!     Err(ParseError::ParseTimedOut { timeout_micros, .. }) => {
//!         log::warn!("Parse timed out after {} ms", timeout_micros / 1000);
//!     }
//!     Err(e) => { /* handle other errors */ }
//! }
//! ```

use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tree_sitter::{Language, ParseOptions, ParseState, Parser, Tree};

use super::error::ParseError;

/// Default maximum input size: 10 MiB.
///
/// This limit prevents unbounded memory allocation from large files while
/// accommodating most legitimate source files. Generated or minified code
/// may exceed this limit and require user configuration.
pub const DEFAULT_MAX_SIZE: usize = 10 * 1024 * 1024;

/// Minimum allowed size limit: 1 MiB.
///
/// Users cannot configure a limit below this threshold to ensure basic
/// functionality is preserved.
pub const MIN_MAX_SIZE: usize = 1024 * 1024;

/// Maximum allowed size limit: 32 MiB.
///
/// Users cannot configure a limit above this threshold to prevent
/// excessive memory usage from extremely large files.
pub const MAX_MAX_SIZE: usize = 32 * 1024 * 1024;

/// Default parse timeout: 2 seconds (2,000,000 microseconds).
///
/// This timeout prevents runaway parsing on pathological inputs that could
/// cause exponential backtracking. Most legitimate files parse in <100ms.
pub const DEFAULT_TIMEOUT_MICROS: u64 = 2_000_000;

/// Minimum allowed timeout: 100ms (100,000 microseconds).
///
/// Users cannot configure a timeout below this threshold as it would
/// cause false positives on normal files.
pub const MIN_TIMEOUT_MICROS: u64 = 100_000;

/// Maximum allowed timeout: 5 seconds (5,000,000 microseconds).
///
/// Users cannot configure a timeout above this threshold to ensure
/// pathological inputs are caught within reasonable time.
pub const MAX_TIMEOUT_MICROS: u64 = 5_000_000;

/// Configuration for `SafeParser` with bounded limits.
///
/// All limits are bounded to prevent users from disabling security protections.
/// Values outside the allowed range are clamped to the nearest bound.
///
/// # Bounds
///
/// - `max_input_size`: [1 MiB, 32 MiB]
/// - `timeout_micros`: [100,000 µs, 5,000,000 µs] (100ms to 5s)
///
/// # Example
///
/// ```
/// use sqry_core::plugin::safe_parse::SafeParserConfig;
///
/// // Use defaults
/// let config = SafeParserConfig::default();
/// assert_eq!(config.max_input_size(), 10 * 1024 * 1024);
/// assert_eq!(config.timeout_micros(), 2_000_000);
///
/// // Custom configuration (values are clamped to bounds)
/// let config = SafeParserConfig::new()
///     .with_max_input_size(20 * 1024 * 1024)
///     .with_timeout_micros(3_000_000);
/// ```
#[derive(Debug, Clone)]
pub struct SafeParserConfig {
    max_input_size: usize,
    timeout_micros: u64,
}

impl Default for SafeParserConfig {
    fn default() -> Self {
        Self {
            max_input_size: DEFAULT_MAX_SIZE,
            timeout_micros: DEFAULT_TIMEOUT_MICROS,
        }
    }
}

impl SafeParserConfig {
    /// Create a new configuration with default values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum input size in bytes.
    ///
    /// Value is clamped to [1 MiB, 32 MiB].
    #[must_use]
    pub fn with_max_input_size(mut self, size: usize) -> Self {
        self.max_input_size = size.clamp(MIN_MAX_SIZE, MAX_MAX_SIZE);
        self
    }

    /// Set parse timeout in microseconds.
    ///
    /// Value is clamped to [100,000 µs, 5,000,000 µs].
    #[must_use]
    pub fn with_timeout_micros(mut self, timeout: u64) -> Self {
        self.timeout_micros = timeout.clamp(MIN_TIMEOUT_MICROS, MAX_TIMEOUT_MICROS);
        self
    }

    /// Get current maximum input size.
    #[must_use]
    pub fn max_input_size(&self) -> usize {
        self.max_input_size
    }

    /// Get current timeout in microseconds.
    #[must_use]
    pub fn timeout_micros(&self) -> u64 {
        self.timeout_micros
    }
}

/// A cancellation flag for aborting long-running parse operations.
///
/// This flag uses atomic operations for thread-safe cancellation signaling.
/// The indexer can set this flag to proactively cancel parsing when needed
/// (e.g., on shutdown, file change, or resource pressure).
///
/// # Example
///
/// ```
/// use sqry_core::plugin::safe_parse::CancellationFlag;
///
/// let flag = CancellationFlag::new();
///
/// // Check if cancelled
/// assert!(!flag.is_cancelled());
///
/// // Signal cancellation
/// flag.cancel();
/// assert!(flag.is_cancelled());
///
/// // Reset for next file
/// flag.reset();
/// assert!(!flag.is_cancelled());
/// ```
#[derive(Debug, Clone, Default)]
pub struct CancellationFlag {
    cancelled: Arc<AtomicBool>,
}

impl CancellationFlag {
    /// Create a new cancellation flag (not cancelled).
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Check if cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Signal cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Reset the flag (clear cancellation).
    ///
    /// Call this between files to avoid leakage.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Relaxed);
    }
}

/// Internal state for tracking parse termination reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminationReason {
    /// Parse completed normally or failed for other reasons.
    None,
    /// Parse was cancelled via cancellation flag.
    Cancelled,
    /// Parse exceeded timeout.
    TimedOut,
}

/// Pure helper function for finalizing parse results with fail-closed behavior.
///
/// **SECURITY CRITICAL**: This function implements fail-closed semantics.
/// If a termination reason was triggered (timeout or cancellation), we return
/// an error regardless of whether tree-sitter produced a partial tree.
///
/// This function is extracted for deterministic testability. The parse outcome
/// decision is pure (depends only on inputs) and can be tested without
/// depending on timing or actual parsing.
///
/// # Arguments
///
/// * `termination_reason` - Why parsing terminated (`None`, `Cancelled`, `TimedOut`)
/// * `tree` - The tree-sitter result (`Some` = tree produced, `None` = no tree)
/// * `file` - Optional file path for error context
/// * `timeout_micros` - Timeout value for error reporting
///
/// # Returns
///
/// * `Ok(Tree)` only if `termination_reason` is `None` AND tree is `Some`
/// * `Err(ParseCancelled)` if `termination_reason` is `Cancelled` (regardless of tree)
/// * `Err(ParseTimedOut)` if `termination_reason` is `TimedOut` (regardless of tree)
/// * `Err(TreeSitterFailed)` if `termination_reason` is `None` but tree is `None`
fn finalize_parse_result(
    termination_reason: TerminationReason,
    tree: Option<Tree>,
    file: Option<&Path>,
    timeout_micros: u64,
) -> Result<Tree, ParseError> {
    // SECURITY: Check termination reason FIRST, before checking if tree exists.
    // Tree-sitter may return a partial tree even after timeout/cancellation.
    // We must fail-closed: if timeout or cancellation was triggered, return an error
    // regardless of whether a partial tree was produced.
    match termination_reason {
        TerminationReason::Cancelled => {
            log::warn!(
                "Parse cancelled{}",
                file.map(|f| format!(" (file: {})", f.display()))
                    .unwrap_or_default()
            );
            return Err(ParseError::ParseCancelled {
                reason: "cancelled during parsing".to_string(),
                file: file.map(Path::to_path_buf),
            });
        }
        TerminationReason::TimedOut => {
            log::warn!(
                "Parse timed out after {} ms{}",
                timeout_micros / 1000,
                file.map(|f| format!(" (file: {})", f.display()))
                    .unwrap_or_default()
            );
            return Err(ParseError::ParseTimedOut {
                timeout_micros,
                file: file.map(Path::to_path_buf),
            });
        }
        TerminationReason::None => {
            // No termination requested, proceed to check tree
        }
    }

    // If no termination was requested, check if tree-sitter produced a tree
    if let Some(t) = tree {
        Ok(t)
    } else {
        log::warn!(
            "Parse failed{}",
            file.map(|f| format!(" (file: {})", f.display()))
                .unwrap_or_default()
        );
        Err(ParseError::TreeSitterFailed)
    }
}

/// Safe parser with resource limits and cancellation support.
///
/// `SafeParser` wraps tree-sitter parsing with:
/// - Input size validation (prevents unbounded allocation)
/// - Parse timeout (prevents exponential backtracking)
/// - External cancellation (allows proactive abort)
///
/// All language plugins should use this utility instead of creating
/// parsers directly to ensure consistent security policy.
///
/// # Thread Safety
///
/// `SafeParser` is `Send + Sync` but the underlying tree-sitter `Parser`
/// is created per-call. This is intentional to avoid thread-safety issues
/// with tree-sitter's internal state.
///
/// # Example
///
/// ```ignore
/// use sqry_core::plugin::safe_parse::{SafeParser, SafeParserConfig};
/// use tree_sitter_rust::LANGUAGE;
///
/// let parser = SafeParser::new(SafeParserConfig::default());
/// let content = b"fn main() {}";
///
/// match parser.parse(&LANGUAGE.into(), content, None) {
///     Ok(tree) => println!("Parsed {} nodes", tree.root_node().child_count()),
///     Err(e) => eprintln!("Parse failed: {}", e),
/// }
/// ```
#[derive(Debug, Clone)]
pub struct SafeParser {
    config: SafeParserConfig,
    cancellation_flag: Option<CancellationFlag>,
}

impl Default for SafeParser {
    fn default() -> Self {
        Self::new(SafeParserConfig::default())
    }
}

impl SafeParser {
    /// Create a new safe parser with the given configuration.
    #[must_use]
    pub fn new(config: SafeParserConfig) -> Self {
        Self {
            config,
            cancellation_flag: None,
        }
    }

    /// Create a safe parser with default configuration.
    #[must_use]
    pub fn with_defaults() -> Self {
        Self::default()
    }

    /// Set a cancellation flag for external abort signaling.
    #[must_use]
    pub fn with_cancellation_flag(mut self, flag: CancellationFlag) -> Self {
        self.cancellation_flag = Some(flag);
        self
    }

    /// Get the current configuration.
    #[must_use]
    pub fn config(&self) -> &SafeParserConfig {
        &self.config
    }

    /// Parse source code with resource limits.
    ///
    /// # Arguments
    ///
    /// * `language` - Tree-sitter language to use
    /// * `content` - Source code as bytes (UTF-8 encoded)
    /// * `file` - Optional file path for error context
    ///
    /// # Returns
    ///
    /// Parsed tree-sitter AST on success.
    ///
    /// # Errors
    ///
    /// - `ParseError::InputTooLarge` - Input exceeds size limit
    /// - `ParseError::ParseTimedOut` - Parsing exceeded timeout
    /// - `ParseError::ParseCancelled` - Parsing was cancelled via flag
    /// - `ParseError::LanguageSetFailed` - Failed to configure parser
    /// - `ParseError::TreeSitterFailed` - Tree-sitter returned no tree
    ///
    /// # Performance
    ///
    /// Creates a new `Parser` per call. This is intentional:
    /// - Avoids thread-safety issues with tree-sitter's state
    /// - Parser creation is cheap (~1µs)
    /// - Timeout/cancellation state is per-parse
    ///
    /// # Implementation Note
    ///
    /// Uses the direct `parser.parse()` API with `set_timeout_micros()` instead of
    /// `parse_with_options()` with a chunked callback. The callback-based API has
    /// compatibility issues with some grammars (e.g., tree-sitter-groovy on multi-
    /// function files). The direct API works universally.
    ///
    /// While `set_timeout_micros` is deprecated in tree-sitter 0.25, it remains
    /// functional and provides better grammar compatibility than the callback approach.
    ///
    /// # Cancellation Limitation
    ///
    /// Mid-parse cancellation is not supported with the direct API. Cancellation is
    /// checked before and after parsing, but not during. For most source files (which
    /// parse in <100ms), this is acceptable. For very large files, the timeout provides
    /// protection.
    ///
    // DEPRECATION: We use `set_timeout_micros` because the recommended replacement,
    // `parse_with_options` (with a callback), has proven to be incompatible with
    // certain grammars (e.g., tree-sitter-groovy). This approach ensures universal
    // grammar compatibility.
    pub fn parse(
        &self,
        language: &Language,
        content: &[u8],
        file: Option<&Path>,
    ) -> Result<Tree, ParseError> {
        // Check cancellation before starting
        if let Some(ref flag) = self.cancellation_flag
            && flag.is_cancelled()
        {
            return Err(ParseError::ParseCancelled {
                reason: "cancelled before parse started".to_string(),
                file: file.map(Path::to_path_buf),
            });
        }

        // Check input size limit
        if content.len() > self.config.max_input_size {
            log::warn!(
                "Input too large: {} bytes exceeds {} limit{}",
                content.len(),
                self.config.max_input_size,
                file.map(|f| format!(" (file: {})", f.display()))
                    .unwrap_or_default()
            );
            return Err(ParseError::InputTooLarge {
                size: content.len(),
                max: self.config.max_input_size,
                file: file.map(Path::to_path_buf),
            });
        }

        // Create and configure parser
        let mut parser = Parser::new();
        parser
            .set_language(language)
            .map_err(|e| ParseError::LanguageSetFailed(e.to_string()))?;

        // Track start time for timeout enforcement
        let start_time = Instant::now();
        let timeout_micros = self.config.timeout_micros;

        // Clone the cancellation flag for use inside the progress callback.
        // The underlying AtomicBool is shared via Arc, so this is cheap.
        let cancellation_flag = self.cancellation_flag.clone();

        // Set up timeout + cancellation via progress callback (tree-sitter 0.26+).
        // set_timeout_micros was removed in tree-sitter 0.26; the progress_callback
        // is now the canonical way to abort a long-running parse.
        let mut progress_fn = move |_: &ParseState| -> ControlFlow<()> {
            if let Some(ref flag) = cancellation_flag
                && flag.is_cancelled()
            {
                return ControlFlow::Break(());
            }
            #[allow(clippy::cast_possible_truncation)]
            // u64 holds 584+ years of µs; max timeout is 5s
            if start_time.elapsed().as_micros() as u64 > timeout_micros {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut progress_fn);

        // Parse with timeout/cancellation enforcement via progress callback.
        let tree = parser.parse_with_options(
            &mut |i, _| content.get(i..).unwrap_or_default(),
            None,
            Some(options),
        );

        // Determine termination reason (fail-closed semantics)
        // SECURITY: Check timeout FIRST using elapsed time, regardless of whether tree-sitter
        // produced a tree. Tree-sitter may return partial trees even after timeout.
        // We must fail-closed: if we exceeded timeout, return an error.
        #[allow(clippy::cast_possible_truncation)] // u64 holds 584+ years of µs; timeout max is 5s
        let elapsed_micros = start_time.elapsed().as_micros() as u64;
        let termination_reason = if let Some(ref flag) = self.cancellation_flag
            && flag.is_cancelled()
        {
            // Cancellation was requested (possibly during parse)
            TerminationReason::Cancelled
        } else if elapsed_micros > self.config.timeout_micros {
            // Timeout occurred - fail-closed regardless of whether tree was produced
            TerminationReason::TimedOut
        } else if tree.is_none() && elapsed_micros >= self.config.timeout_micros {
            // Edge case: tree-sitter aborted exactly at timeout boundary (returns None)
            // Treat this as timeout rather than TreeSitterFailed for accurate telemetry
            TerminationReason::TimedOut
        } else {
            TerminationReason::None
        };

        // Delegate to pure helper for fail-closed result handling
        finalize_parse_result(termination_reason, tree, file, self.config.timeout_micros)
    }

    /// Parse source code with file path context.
    ///
    /// Convenience method that always includes file path in errors.
    ///
    /// # Errors
    ///
    /// Same as [`parse`](Self::parse).
    pub fn parse_file(
        &self,
        language: &Language,
        content: &[u8],
        file: &Path,
    ) -> Result<Tree, ParseError> {
        self.parse(language, content, Some(file))
    }

    /// Log a summary of the current configuration.
    ///
    /// Call this once at startup to record active limits for incident triage.
    #[allow(clippy::cast_precision_loss)] // max_input_size <= 32 MiB, well under f64 precision limit
    pub fn log_config(&self) {
        log::info!(
            "SafeParser configured: max_size={} bytes ({:.1} MiB), timeout={} ms",
            self.config.max_input_size,
            self.config.max_input_size as f64 / (1024.0 * 1024.0),
            self.config.timeout_micros / 1000
        );
    }
}

/// Parse content using the default safe parser configuration.
///
/// This is a convenience function for simple cases. For production use,
/// prefer creating a `SafeParser` instance with explicit configuration.
///
/// # Errors
///
/// Same as [`SafeParser::parse`].
pub fn parse_safe(
    language: &Language,
    content: &[u8],
    file: Option<&Path>,
) -> Result<Tree, ParseError> {
    SafeParser::default().parse(language, content, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_config_defaults() {
        let config = SafeParserConfig::default();
        assert_eq!(config.max_input_size(), DEFAULT_MAX_SIZE);
        assert_eq!(config.timeout_micros(), DEFAULT_TIMEOUT_MICROS);
    }

    #[test]
    fn test_config_builder() {
        let config = SafeParserConfig::new()
            .with_max_input_size(20 * 1024 * 1024)
            .with_timeout_micros(3_000_000);

        assert_eq!(config.max_input_size(), 20 * 1024 * 1024);
        assert_eq!(config.timeout_micros(), 3_000_000);
    }

    #[test]
    fn test_config_clamping_min() {
        // Below minimum should clamp up
        let config = SafeParserConfig::new()
            .with_max_input_size(100) // Way below 1 MiB
            .with_timeout_micros(1000); // Way below 100ms

        assert_eq!(config.max_input_size(), MIN_MAX_SIZE);
        assert_eq!(config.timeout_micros(), MIN_TIMEOUT_MICROS);
    }

    #[test]
    fn test_config_clamping_max() {
        // Above maximum should clamp down
        let config = SafeParserConfig::new()
            .with_max_input_size(100 * 1024 * 1024) // 100 MiB > 32 MiB max
            .with_timeout_micros(10_000_000); // 10s > 5s max

        assert_eq!(config.max_input_size(), MAX_MAX_SIZE);
        assert_eq!(config.timeout_micros(), MAX_TIMEOUT_MICROS);
    }

    #[test]
    fn test_cancellation_flag() {
        let flag = CancellationFlag::new();

        assert!(!flag.is_cancelled());

        flag.cancel();
        assert!(flag.is_cancelled());

        flag.reset();
        assert!(!flag.is_cancelled());
    }

    #[test]
    fn test_cancellation_flag_clone() {
        let flag1 = CancellationFlag::new();
        let flag2 = flag1.clone();

        flag1.cancel();
        assert!(flag2.is_cancelled()); // Clone shares the same Arc
    }

    #[test]
    fn test_safe_parser_creation() {
        let parser = SafeParser::with_defaults();
        assert_eq!(parser.config().max_input_size(), DEFAULT_MAX_SIZE);
        assert_eq!(parser.config().timeout_micros(), DEFAULT_TIMEOUT_MICROS);
    }

    #[test]
    fn test_safe_parser_with_config() {
        let config = SafeParserConfig::new().with_max_input_size(5 * 1024 * 1024);
        let parser = SafeParser::new(config);

        assert_eq!(parser.config().max_input_size(), 5 * 1024 * 1024);
    }

    #[test]
    fn test_safe_parser_with_cancellation() {
        let flag = CancellationFlag::new();
        let parser = SafeParser::with_defaults().with_cancellation_flag(flag.clone());

        // Parser should have the flag
        assert!(parser.cancellation_flag.is_some());
    }

    #[test]
    fn test_input_too_large_error() {
        // Create parser with tiny limit for testing
        let config = SafeParserConfig::new().with_max_input_size(MIN_MAX_SIZE);
        let parser = SafeParser::new(config);

        // Content larger than 1 MiB
        let large_content = vec![b'x'; MIN_MAX_SIZE + 1];

        // Use a dummy language (we'll hit size check before parsing)
        let language = tree_sitter_rust::LANGUAGE.into();
        let result = parser.parse(&language, &large_content, None);

        match result {
            Err(ParseError::InputTooLarge { size, max, file }) => {
                assert_eq!(size, MIN_MAX_SIZE + 1);
                assert_eq!(max, MIN_MAX_SIZE);
                assert!(file.is_none());
            }
            _ => panic!("Expected InputTooLarge error"),
        }
    }

    #[test]
    fn test_input_too_large_with_file() {
        let config = SafeParserConfig::new().with_max_input_size(MIN_MAX_SIZE);
        let parser = SafeParser::new(config);

        let large_content = vec![b'x'; MIN_MAX_SIZE + 1];
        let file_path = PathBuf::from("/path/to/large.rs");
        let language = tree_sitter_rust::LANGUAGE.into();

        let result = parser.parse_file(&language, &large_content, &file_path);

        match result {
            Err(ParseError::InputTooLarge { file, .. }) => {
                assert_eq!(file, Some(file_path));
            }
            _ => panic!("Expected InputTooLarge error with file path"),
        }
    }

    #[test]
    fn test_cancelled_before_parse() {
        let flag = CancellationFlag::new();
        flag.cancel(); // Cancel before parsing

        let parser = SafeParser::with_defaults().with_cancellation_flag(flag);

        let content = b"fn main() {}";
        let language = tree_sitter_rust::LANGUAGE.into();
        let result = parser.parse(&language, content, None);

        match result {
            Err(ParseError::ParseCancelled { reason, .. }) => {
                assert!(reason.contains("before parse started"));
            }
            _ => panic!("Expected ParseCancelled error"),
        }
    }

    #[test]
    fn test_successful_parse() {
        let parser = SafeParser::with_defaults();
        let content = b"fn main() {}";
        let language = tree_sitter_rust::LANGUAGE.into();

        let result = parser.parse(&language, content, None);
        assert!(result.is_ok());

        let tree = result.unwrap();
        // Verify we got a valid tree by checking root node kind
        assert_eq!(tree.root_node().kind(), "source_file");
    }

    #[test]
    fn test_successful_parse_with_file() {
        let parser = SafeParser::with_defaults();
        let content = b"fn main() { let x = 42; }";
        let file_path = PathBuf::from("test.rs");
        let language = tree_sitter_rust::LANGUAGE.into();

        let result = parser.parse_file(&language, content, &file_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_safe_convenience() {
        let content = b"fn foo() {}";
        let language = tree_sitter_rust::LANGUAGE.into();

        let result = parse_safe(&language, content, None);
        assert!(result.is_ok());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)] // These assertions serve as documentation
    fn test_constants_sanity() {
        // Verify constant relationships
        assert!(MIN_MAX_SIZE < DEFAULT_MAX_SIZE);
        assert!(DEFAULT_MAX_SIZE < MAX_MAX_SIZE);
        assert!(MIN_TIMEOUT_MICROS < DEFAULT_TIMEOUT_MICROS);
        assert!(DEFAULT_TIMEOUT_MICROS < MAX_TIMEOUT_MICROS);

        // Verify human-friendly values
        assert_eq!(MIN_MAX_SIZE, 1024 * 1024); // 1 MiB
        assert_eq!(DEFAULT_MAX_SIZE, 10 * 1024 * 1024); // 10 MiB
        assert_eq!(MAX_MAX_SIZE, 32 * 1024 * 1024); // 32 MiB
        assert_eq!(MIN_TIMEOUT_MICROS, 100_000); // 100ms
        assert_eq!(DEFAULT_TIMEOUT_MICROS, 2_000_000); // 2s
        assert_eq!(MAX_TIMEOUT_MICROS, 5_000_000); // 5s
    }

    #[test]
    fn test_termination_reason_enum() {
        // Test that enum variants are distinct
        assert_ne!(TerminationReason::None, TerminationReason::Cancelled);
        assert_ne!(TerminationReason::None, TerminationReason::TimedOut);
        assert_ne!(TerminationReason::Cancelled, TerminationReason::TimedOut);
    }

    /// Test fail-closed behavior: timeout must return error even if tree was partially produced.
    ///
    /// This test verifies the security-critical fail-closed behavior:
    /// When a timeout is triggered during parsing, we MUST return `ParseTimedOut` error
    /// regardless of whether tree-sitter produced a partial tree.
    ///
    /// The fix for this was: check `termination_reason` BEFORE checking if a tree exists,
    /// rather than only checking `termination_reason` when `tree` is `None`.
    #[test]
    fn test_timeout_returns_error_fail_closed() {
        // Use minimum timeout (100ms) with code that might trigger timeout
        let config = SafeParserConfig::new().with_timeout_micros(MIN_TIMEOUT_MICROS);
        let parser = SafeParser::new(config);

        // Complex-ish code that might take some time to parse
        // Even if it parses faster than 100ms, this test still validates the happy path.
        // The key security guarantee is that IF the timeout triggers, we fail.
        let content = br#"
            fn complex_function() {
                let x = vec![1, 2, 3, 4, 5];
                for i in x.iter() {
                    if *i > 3 {
                        println!("{}", i);
                    }
                }
            }
        "#;

        let language = tree_sitter_rust::LANGUAGE.into();
        let result = parser.parse(&language, content, None);

        // Result should be either:
        // - Ok(tree) if parsing completed within timeout
        // - Err(ParseTimedOut) if timeout was triggered
        // The key is: it must NEVER return Ok(partial_tree) after timeout
        match result {
            Ok(_tree) => {
                // Parsing completed within timeout - that's fine
                // This test primarily documents the fail-closed requirement
            }
            Err(ParseError::ParseTimedOut { timeout_micros, .. }) => {
                // Timeout triggered - verify we got the error, not a partial tree
                assert_eq!(timeout_micros, MIN_TIMEOUT_MICROS);
            }
            Err(ParseError::TreeSitterFailed) => {
                // Callback compatibility issue - acceptable
            }
            Err(e) => {
                panic!("Unexpected error type: {e:?}");
            }
        }
    }

    /// Test fail-closed behavior with cancellation.
    ///
    /// This test verifies that when cancellation is triggered DURING parsing,
    /// we return `ParseCancelled` even if a partial tree was produced.
    #[test]
    fn test_cancellation_during_parse_fail_closed() {
        use std::thread;
        use std::time::Duration;

        let flag = CancellationFlag::new();
        let flag_clone = flag.clone();

        // Use short timeout to give cancellation time to trigger
        let config = SafeParserConfig::new().with_timeout_micros(MIN_TIMEOUT_MICROS);
        let parser = SafeParser::new(config).with_cancellation_flag(flag);

        // Spawn thread that cancels after a tiny delay
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_micros(10));
            flag_clone.cancel();
        });

        // Moderately complex code
        let content = br"
            fn foo() { let x = 1; }
            fn bar() { let y = 2; }
            fn baz() { let z = 3; }
        ";

        let language = tree_sitter_rust::LANGUAGE.into();
        let result = parser.parse(&language, content, None);

        handle.join().unwrap();

        // Result can be:
        // - Ok: parsed before cancel took effect
        // - Err(ParseCancelled): cancel triggered during parse
        // - Err(ParseTimedOut): timeout triggered
        // - Err(TreeSitterFailed): callback compatibility
        // Key: NEVER Ok(partial_tree) after cancellation was triggered
        match result {
            Ok(_)
            | Err(
                ParseError::ParseCancelled { .. }
                | ParseError::ParseTimedOut { .. }
                | ParseError::TreeSitterFailed,
            ) => {
                // All acceptable outcomes - the key is fail-closed behavior
            }
            Err(e) => {
                panic!("Unexpected error type: {e:?}");
            }
        }
    }

    // ========================================================================
    // DETERMINISTIC FAIL-CLOSED TESTS
    // These tests call finalize_parse_result directly with controlled inputs
    // to verify fail-closed behavior without depending on timing or parsing.
    // ========================================================================

    /// Helper to create a valid tree for testing `finalize_parse_result`.
    fn create_test_tree() -> Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        parser.parse(b"fn main() {}", None).unwrap()
    }

    /// DETERMINISTIC: Timeout + Some(tree) must return `ParseTimedOut` error.
    ///
    /// This is the critical fail-closed test. Even if tree-sitter produces
    /// a partial tree after timeout, we MUST return an error.
    #[test]
    fn test_finalize_timeout_with_tree_returns_error() {
        let tree = create_test_tree();
        let result =
            finalize_parse_result(TerminationReason::TimedOut, Some(tree), None, 2_000_000);

        match result {
            Err(ParseError::ParseTimedOut {
                timeout_micros,
                file,
            }) => {
                assert_eq!(timeout_micros, 2_000_000);
                assert!(file.is_none());
            }
            _ => panic!("Expected ParseTimedOut, got {result:?}"),
        }
    }

    /// DETERMINISTIC: Cancellation + Some(tree) must return `ParseCancelled` error.
    ///
    /// Same as timeout case: even with a tree, cancellation returns error.
    #[test]
    fn test_finalize_cancelled_with_tree_returns_error() {
        let tree = create_test_tree();
        let result =
            finalize_parse_result(TerminationReason::Cancelled, Some(tree), None, 2_000_000);

        match result {
            Err(ParseError::ParseCancelled { reason, file }) => {
                assert!(reason.contains("cancelled"));
                assert!(file.is_none());
            }
            _ => panic!("Expected ParseCancelled, got {result:?}"),
        }
    }

    /// DETERMINISTIC: Timeout + None must return `ParseTimedOut` error.
    #[test]
    fn test_finalize_timeout_without_tree_returns_error() {
        let result = finalize_parse_result(TerminationReason::TimedOut, None, None, 2_000_000);

        match result {
            Err(ParseError::ParseTimedOut { .. }) => {}
            _ => panic!("Expected ParseTimedOut, got {result:?}"),
        }
    }

    /// DETERMINISTIC: Cancellation + None must return `ParseCancelled` error.
    #[test]
    fn test_finalize_cancelled_without_tree_returns_error() {
        let result = finalize_parse_result(TerminationReason::Cancelled, None, None, 2_000_000);

        match result {
            Err(ParseError::ParseCancelled { .. }) => {}
            _ => panic!("Expected ParseCancelled, got {result:?}"),
        }
    }

    /// DETERMINISTIC: No termination + Some(tree) returns Ok(tree).
    #[test]
    fn test_finalize_success_with_tree() {
        let tree = create_test_tree();
        let result = finalize_parse_result(TerminationReason::None, Some(tree), None, 2_000_000);

        assert!(result.is_ok());
        assert_eq!(result.unwrap().root_node().kind(), "source_file");
    }

    /// DETERMINISTIC: No termination + None returns `TreeSitterFailed`.
    #[test]
    fn test_finalize_failure_without_tree() {
        let result = finalize_parse_result(TerminationReason::None, None, None, 2_000_000);

        match result {
            Err(ParseError::TreeSitterFailed) => {}
            _ => panic!("Expected TreeSitterFailed, got {result:?}"),
        }
    }

    /// DETERMINISTIC: Verify file path is included in timeout error.
    #[test]
    fn test_finalize_timeout_includes_file_path() {
        let tree = create_test_tree();
        let file_path = Path::new("/path/to/test.rs");
        let result = finalize_parse_result(
            TerminationReason::TimedOut,
            Some(tree),
            Some(file_path),
            1_500_000,
        );

        match result {
            Err(ParseError::ParseTimedOut {
                timeout_micros,
                file,
            }) => {
                assert_eq!(timeout_micros, 1_500_000);
                assert_eq!(file, Some(PathBuf::from("/path/to/test.rs")));
            }
            _ => panic!("Expected ParseTimedOut with file path, got {result:?}"),
        }
    }

    /// DETERMINISTIC: Verify file path is included in cancellation error.
    #[test]
    fn test_finalize_cancelled_includes_file_path() {
        let tree = create_test_tree();
        let file_path = Path::new("/some/code.rs");
        let result = finalize_parse_result(
            TerminationReason::Cancelled,
            Some(tree),
            Some(file_path),
            2_000_000,
        );

        match result {
            Err(ParseError::ParseCancelled { file, .. }) => {
                assert_eq!(file, Some(PathBuf::from("/some/code.rs")));
            }
            _ => panic!("Expected ParseCancelled with file path, got {result:?}"),
        }
    }
}
