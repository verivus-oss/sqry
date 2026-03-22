//! Type definitions for local uses and insights
//!
//! This module contains the AUTHORITATIVE type definitions for all uses/insights
//! data structures. All fields use strongly-typed enums and validated newtypes -
//! there are no `String`, `HashMap<String, _>`, or `Vec<String>` fields where
//! arbitrary user content could leak.
//!
//! # Privacy Guarantees
//!
//! Privacy is enforced at compile time through the type system:
//! - All event fields are enums or validated newtypes
//! - Adding a `String` field requires explicit review
//! - No arbitrary user content can leak into telemetry
//!
//! # Serialization
//!
//! All types use `snake_case` JSON serialization for stable CLI-facing output:
//! - C-like enums: `#[serde(rename_all = "snake_case")]`
//! - Sum enums with data: `#[serde(tag = "type", rename_all = "snake_case")]`

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::time::Instant;

// ============================================================================
// Core Event Types
// ============================================================================

/// A single use event captured by sqry
///
/// Events describe what sqry did, not what the user's code contains.
/// All fields are strongly typed - no arbitrary strings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UseEvent {
    /// When the event occurred (ISO 8601)
    pub timestamp: DateTime<Utc>,
    /// Type-specific event data (tagged enum)
    pub event_type: UseEventType,
    /// How long the operation took in milliseconds (None if not measured)
    pub duration_ms: Option<u64>,
    // NO metadata HashMap - all data is in event_type variants
}

impl UseEvent {
    /// Create a new event with the current timestamp
    #[must_use]
    pub fn new(event_type: UseEventType) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type,
            duration_ms: None,
        }
    }

    /// Create a new event with a specific duration
    #[must_use]
    pub fn with_duration(event_type: UseEventType, duration_ms: u64) -> Self {
        Self {
            timestamp: Utc::now(),
            event_type,
            duration_ms: Some(duration_ms),
        }
    }

    /// Create a test event for use in tests
    #[cfg(test)]
    #[must_use]
    pub fn test_event() -> Self {
        Self::new(UseEventType::QueryExecuted {
            kind: QueryKind::CallChain,
            result_count: 42,
        })
    }
}

/// Sum enum with data variants - uses internally tagged serialization.
///
/// Serializes as: `{ "type": "query_executed", "kind": "impact_analysis", ... }`
///
/// Note: Internally tagged (`#[serde(tag = "type")]`) is for sum enums WITH data.
/// C-like enums use plain `#[serde(rename_all)]` only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UseEventType {
    /// Query completed successfully
    QueryExecuted {
        /// The type of query executed
        kind: QueryKind,
        /// Number of results returned
        result_count: usize,
    },
    /// Graph traversal performed
    GraphExpanded {
        /// Type of graph expanded
        kind: GraphKind,
        /// Depth of expansion
        depth: u8,
    },
    /// AI answer generated
    AiAnswerGenerated {
        /// Whether the user accepted the answer
        accepted: bool,
        /// Whether the user re-queried after this answer
        requeried: bool,
    },
    /// User abandoned a view before completing
    ViewAbandoned {
        /// Type of view abandoned
        kind: ViewKind,
        /// Time spent in the view before abandoning (ms)
        time_spent_ms: u64,
    },
    /// Data exported to external format
    ExportGenerated {
        /// Format of the export
        format: ExportFormat,
    },
    /// User provided feedback
    FeedbackProvided {
        /// Context in which feedback was given
        context: FeedbackContext,
        /// The user's response
        response: FeedbackResponse,
    },
}

// ============================================================================
// C-Like Enum Types (no data variants - use plain rename_all, NOT tagged)
// These serialize as simple strings: "call_chain", "linux", etc.
// ============================================================================

/// Types of queries that can be executed
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// Call chain traversal
    CallChain,
    /// Impact/blast radius analysis
    ImpactAnalysis,
    /// Node lookup
    SymbolLookup,
    /// Semantic search
    Semantic,
    /// Unused code detection
    Unused,
    /// Duplicate code detection
    Duplicates,
    /// Circular dependency detection
    Circular,
}

/// Types of graphs that can be expanded
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GraphKind {
    /// Call graph showing function calls
    CallGraph,
    /// Dependency graph showing module dependencies
    DependencyGraph,
    /// Import graph showing import relationships
    ImportGraph,
}

/// Types of views that can be displayed
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    /// Graph visualization
    Graph,
    /// List view
    List,
    /// Tree view
    Tree,
    /// Detailed view
    Detail,
}

/// Supported export formats
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// JSON format
    Json,
    /// Graphviz DOT format
    Dot,
    /// Mermaid diagram format
    Mermaid,
    /// D2 diagram format
    D2,
}

/// Context in which feedback was requested
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackContext {
    /// User abandoned a view early
    AbandonedView,
    /// User re-queried after an AI answer
    RequeriedAnswer,
    /// Result took too long
    SlowResult,
}

/// User feedback response options
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackResponse {
    /// Result was unclear
    Unclear,
    /// Missing necessary context
    MissingContext,
    /// Operation was too slow
    TooSlow,
    /// Result was not useful
    NotUseful,
    /// User dismissed the feedback prompt
    Dismissed,
}

// ============================================================================
// Diagnostics Summary Types
// ============================================================================

/// Workflow count entry for diagnostics summary
///
/// Serializes as: `{ "kind": "impact_analysis", "count": 42 }`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopWorkflow {
    /// The type of workflow
    pub kind: QueryKind,
    /// Number of times this workflow was used
    pub count: usize,
}

/// Per-graph-kind abandonment rate for detailed insights
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphAbandonRate {
    /// The type of graph
    pub kind: GraphKind,
    /// Abandonment rate (0.0 to 1.0)
    pub rate: f64,
}

/// Weekly diagnostics summary - aggregated from daily event logs
///
/// Timing metrics measure operation duration (seconds from request start to first result):
/// - avg/median typically 0.5-3.0s for indexed queries
/// - Higher values may indicate cold cache or complex graphs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsSummary {
    /// The period this summary covers (ISO week format)
    pub period: IsoWeekPeriod,
    /// Top workflows by usage count (struct with kind/count, not tuple)
    pub top_workflows: Vec<TopWorkflow>,
    /// Mean operation duration in seconds
    pub avg_time_to_result_sec: f64,
    /// Median operation duration in seconds (less skew-sensitive)
    pub median_time_to_result_sec: f64,
    /// Overall abandonment rate (0.0 to 1.0)
    pub abandon_rate: f64,
    /// Per-kind abandonment for detailed reports
    pub abandonment: Vec<GraphAbandonRate>,
    /// Rate at which AI answers were re-queried (0.0 to 1.0)
    pub ai_requery_rate: f64,
    /// Total number of use events in this period
    pub total_uses: usize,
    /// Number of events dropped due to backpressure
    pub dropped_events: u64,
}

impl Default for DiagnosticsSummary {
    fn default() -> Self {
        Self {
            period: IsoWeekPeriod::current(),
            top_workflows: Vec::new(),
            avg_time_to_result_sec: 0.0,
            median_time_to_result_sec: 0.0,
            abandon_rate: 0.0,
            abandonment: Vec::new(),
            ai_requery_rate: 0.0,
            total_uses: 0,
            dropped_events: 0,
        }
    }
}

// ============================================================================
// Validated Newtypes
// ============================================================================

// Pre-compiled regex for ISO week validation (initialized once at first use)
// Using Lazy ensures the regex is compiled only once. The .expect() will panic
// at static initialization if the pattern is invalid - not at compile time,
// but guaranteed to fail immediately on first access with a known-good literal.
static ISO_WEEK_REGEX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"^\d{4}-W(0[1-9]|[1-4]\d|5[0-3])$")
        .expect("ISO week regex is valid - literal pattern guaranteed at static initialization")
});

/// Newtype for ISO week periods - validated format only
///
/// Format: "2025-W50" (year-Wweek)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IsoWeekPeriod(String);

impl IsoWeekPeriod {
    /// Create a new ISO week period with validation
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not in valid ISO week format (YYYY-Www).
    pub fn try_new(s: &str) -> Result<Self, &'static str> {
        // Use pre-compiled regex (no runtime panic risk)
        if ISO_WEEK_REGEX.is_match(s) {
            Ok(Self(s.to_string()))
        } else {
            Err("invalid ISO week format - expected YYYY-Www (e.g., 2025-W50)")
        }
    }

    /// Get the current ISO week period
    #[must_use]
    pub fn current() -> Self {
        let now = Utc::now();
        let week = now.format("%G-W%V").to_string();
        Self(week)
    }

    /// Get the inner string value
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IsoWeekPeriod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Newtype for sqry version - validated semver format only
///
/// Format: "0.5.0" (MAJOR.MINOR.PATCH with optional pre-release)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SqryVersion(String);

impl SqryVersion {
    /// Create a new version with semver validation
    ///
    /// # Errors
    ///
    /// Returns an error if the string is not valid semver format.
    pub fn try_new(s: &str) -> Result<Self, &'static str> {
        // Validate semver format: MAJOR.MINOR.PATCH with optional pre-release
        if semver::Version::parse(s).is_ok() {
            Ok(Self(s.to_string()))
        } else {
            Err("invalid semver format - expected MAJOR.MINOR.PATCH (e.g., 0.5.0)")
        }
    }

    /// Get the current sqry version from Cargo.toml
    #[must_use]
    pub fn current() -> Self {
        Self(env!("CARGO_PKG_VERSION").to_string())
    }

    /// Get the inner string value
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SqryVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// Share Snapshot (for --share output)
// ============================================================================

/// Share snapshot - self-contained payload for `sqry insights --share`
///
/// Flattens `DiagnosticsSummary` with metadata for anonymous sharing.
/// This is the canonical format for shareable insights files.
///
/// All fields are either validated string newtypes (`SqryVersion`,
/// `IsoWeekPeriod`), C-like enums (`QueryKind`, `GraphKind`), or numerics.
/// The optional `merged_period` field contains only `"YYYY-Www..YYYY-Www"`
/// format strings constructed programmatically from validated `IsoWeekPeriod`
/// values.  There are no free-form `String` or `PathBuf` escape hatches —
/// privacy is structural.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShareSnapshot {
    /// Sqry version that generated this snapshot
    pub sqry_version: SqryVersion,
    /// Week identifier (always a single valid ISO week, e.g. `"2026-W09"`)
    pub period: IsoWeekPeriod,
    /// Top workflows from `DiagnosticsSummary`
    pub top_workflows: Vec<TopWorkflow>,
    /// Mean operation duration in seconds
    pub avg_time_to_result_sec: f64,
    /// Median operation duration in seconds
    pub median_time_to_result_sec: f64,
    /// Overall abandonment rate
    pub abandon_rate: f64,
    /// Per-kind abandonment rates
    pub abandonment: Vec<GraphAbandonRate>,
    /// AI requery rate
    pub ai_requery_rate: f64,
    /// Total use events
    pub total_uses: usize,
    /// Dropped events count
    pub dropped_events: u64,
    /// Present only for merged multi-week snapshots.
    ///
    /// Format: `"YYYY-Www..YYYY-Www"` (first week .. last week).
    /// `None` for single-week snapshots.
    /// `period` always holds the first week's `IsoWeekPeriod`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_period: Option<String>,
}

impl ShareSnapshot {
    /// Create a share snapshot from a diagnostics summary
    #[must_use]
    pub fn from_summary(summary: &DiagnosticsSummary) -> Self {
        Self {
            sqry_version: SqryVersion::current(),
            period: summary.period.clone(),
            top_workflows: summary.top_workflows.clone(),
            avg_time_to_result_sec: summary.avg_time_to_result_sec,
            median_time_to_result_sec: summary.median_time_to_result_sec,
            abandon_rate: summary.abandon_rate,
            abandonment: summary.abandonment.clone(),
            ai_requery_rate: summary.ai_requery_rate,
            total_uses: summary.total_uses,
            dropped_events: summary.dropped_events,
            merged_period: None,
        }
    }
}

// ============================================================================
// Troubleshoot Types
// ============================================================================

/// Sanitized config for troubleshoot bundles - no paths, no secrets
///
/// Only includes safe, non-identifying configuration values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SanitizedConfig {
    /// Whether uses capture is enabled
    pub uses_enabled: bool,
    /// Cache entry limit (numeric, not path)
    pub cache_size: usize,
}

/// System information - enum-only, no version strings
///
/// Captures high-level platform info without identifying details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    /// Operating system kind (enum, not version string)
    pub os: OsKind,
    /// Architecture kind (enum, not specific CPU model)
    pub arch: ArchKind,
    /// Sqry version (validated newtype)
    pub sqry_version: SqryVersion,
    /// Build type (release/debug)
    pub sqry_build: BuildKind,
}

impl SystemInfo {
    /// Create system info for the current environment
    #[must_use]
    pub fn current() -> Self {
        Self {
            os: OsKind::current(),
            arch: ArchKind::current(),
            sqry_version: SqryVersion::current(),
            sqry_build: BuildKind::current(),
        }
    }
}

/// Operating system kinds
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OsKind {
    /// Linux
    Linux,
    /// macOS
    MacOs,
    /// Windows
    Windows,
    /// FreeBSD
    FreeBsd,
    /// Other/unknown OS
    Other,
}

impl OsKind {
    /// Detect the current OS
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Self::Linux,
            "macos" => Self::MacOs,
            "windows" => Self::Windows,
            "freebsd" => Self::FreeBsd,
            _ => Self::Other,
        }
    }
}

/// Architecture kinds
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArchKind {
    /// x86-64 / AMD64
    X86_64,
    /// ARM64 / `AArch64`
    Aarch64,
    /// 32-bit ARM
    Arm,
    /// Other/unknown architecture
    Other,
}

impl ArchKind {
    /// Detect the current architecture
    #[must_use]
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            "arm" => Self::Arm,
            _ => Self::Other,
        }
    }
}

/// Build type kinds
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildKind {
    /// Release build
    Release,
    /// Debug build
    Debug,
    /// Custom build configuration
    Custom,
}

impl BuildKind {
    /// Detect the current build type
    #[must_use]
    pub fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }
}

/// Troubleshoot bundle - structured data for issue reporting
///
/// All fields are type-safe, no arbitrary strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TroubleshootBundle {
    /// When the bundle was generated
    pub generated_at: DateTime<Utc>,
    /// Sqry version (validated semver newtype)
    pub sqry_version: SqryVersion,
    /// System information
    pub system_info: SystemInfo,
    /// Sanitized configuration
    pub config_sanitized: SanitizedConfig,
    /// Recent use events (last 24h) - already type-safe
    pub recent_uses: Vec<UseEvent>,
    /// Recent structured errors - NO raw strings
    pub recent_errors: Vec<StructuredError>,
    /// Workflow trace (explicit opt-in with preview)
    pub workflow_trace: Option<WorkflowTrace>,
    /// Backpressure visibility counter
    pub dropped_events: u64,
}

/// Structured error - no arbitrary strings, no paths
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StructuredError {
    /// When the error occurred
    pub timestamp: DateTime<Utc>,
    /// Error kind (enum, not string)
    pub kind: ErrorKind,
    /// Error category (enum, not string)
    pub category: ErrorCategory,
    /// Whether the operation can be retried
    pub retryable: bool,
    /// Number of occurrences
    pub count: usize,
}

/// Error kinds for troubleshoot bundles
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Parse failure
    ParseError,
    /// Index corruption or error
    IndexError,
    /// Query execution error
    QueryError,
    /// I/O error
    IoError,
    /// Configuration error
    ConfigError,
    /// Plugin error
    PluginError,
    /// Unknown error type
    Unknown,
}

/// Error categories for troubleshoot bundles
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// User-caused error (bad input)
    User,
    /// System error (resource issues)
    System,
    /// Network error
    Network,
    /// Internal sqry error
    Internal,
}

/// Workflow trace - strictly typed enum steps (opt-in via --include-trace)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowTrace {
    /// Sequence of workflow steps
    pub steps: Vec<WorkflowStep>,
}

/// Workflow step enum - tagged serialization like `UseEventType`
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkflowStep {
    /// Query started
    QueryStarted {
        /// Type of query
        kind: QueryKind,
    },
    /// Graph expanded
    GraphExpanded {
        /// Type of graph
        kind: GraphKind,
        /// Depth of expansion
        depth: u8,
    },
    /// Results displayed to user
    ResultsDisplayed {
        /// Number of results shown
        count: usize,
    },
    /// Export generated
    ExportGenerated {
        /// Format of export
        format: ExportFormat,
    },
    /// Session ended
    SessionEnded,
}

// ============================================================================
// Timer Helper
// ============================================================================

/// RAII timer for recording event duration on drop
///
/// Used to automatically capture operation duration without manual timing code.
pub struct TimedUse {
    start: Instant,
    event_type: Option<UseEventType>,
}

impl TimedUse {
    /// Create a new timer for the given event type
    #[must_use]
    pub fn new(event_type: UseEventType) -> Self {
        Self {
            start: Instant::now(),
            event_type: Some(event_type),
        }
    }

    /// Complete the timer and return the event with duration
    ///
    /// # Panics
    /// Panics if the timer has already been consumed.
    #[must_use]
    pub fn finish(mut self) -> UseEvent {
        let event_type = self
            .event_type
            .take()
            .expect("TimedUse event_type should be present");
        let duration_ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        UseEvent::with_duration(event_type, duration_ms)
    }

    /// Cancel the timer without recording an event
    pub fn cancel(mut self) {
        self.event_type = None;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    const FLOAT_EPSILON: f64 = 1.0e-9;

    fn assert_json_eq<T: Serialize>(value: &T, expected: &str) {
        assert_eq!(serde_json::to_string(value).unwrap(), expected);
    }

    fn assert_f64_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < FLOAT_EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn test_use_event_serialization() {
        let event = UseEvent {
            timestamp: DateTime::parse_from_rfc3339("2025-12-13T14:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            event_type: UseEventType::QueryExecuted {
                kind: QueryKind::CallChain,
                result_count: 42,
            },
            duration_ms: Some(123),
        };

        let json = serde_json::to_string(&event).unwrap();
        let parsed: UseEvent = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.event_type, event.event_type);
        assert_eq!(parsed.duration_ms, event.duration_ms);
    }

    #[test]
    fn test_use_event_type_tagged_serialization() {
        let event_type = UseEventType::QueryExecuted {
            kind: QueryKind::ImpactAnalysis,
            result_count: 10,
        };

        let json = serde_json::to_string(&event_type).unwrap();

        // Should use internally tagged format
        assert!(json.contains(r#""type":"query_executed""#));
        assert!(json.contains(r#""kind":"impact_analysis""#));
        assert!(json.contains(r#""result_count":10"#));
    }

    #[test]
    fn test_query_kind_serialization() {
        assert_json_eq(&QueryKind::CallChain, "\"call_chain\"");
        assert_json_eq(&QueryKind::ImpactAnalysis, "\"impact_analysis\"");
        assert_json_eq(&QueryKind::SymbolLookup, "\"symbol_lookup\"");
        assert_json_eq(&QueryKind::Semantic, "\"semantic\"");
        assert_json_eq(&QueryKind::Unused, "\"unused\"");
        assert_json_eq(&QueryKind::Duplicates, "\"duplicates\"");
        assert_json_eq(&QueryKind::Circular, "\"circular\"");
    }

    #[test]
    fn test_graph_kind_serialization() {
        assert_json_eq(&GraphKind::CallGraph, "\"call_graph\"");
        assert_json_eq(&GraphKind::DependencyGraph, "\"dependency_graph\"");
        assert_json_eq(&GraphKind::ImportGraph, "\"import_graph\"");
    }

    #[test]
    fn test_view_kind_serialization() {
        assert_json_eq(&ViewKind::Graph, "\"graph\"");
        assert_json_eq(&ViewKind::List, "\"list\"");
        assert_json_eq(&ViewKind::Tree, "\"tree\"");
        assert_json_eq(&ViewKind::Detail, "\"detail\"");
    }

    #[test]
    fn test_export_format_serialization() {
        assert_json_eq(&ExportFormat::Json, "\"json\"");
        assert_json_eq(&ExportFormat::Dot, "\"dot\"");
        assert_json_eq(&ExportFormat::Mermaid, "\"mermaid\"");
        assert_json_eq(&ExportFormat::D2, "\"d2\"");
    }

    #[test]
    fn test_feedback_context_serialization() {
        assert_json_eq(&FeedbackContext::AbandonedView, "\"abandoned_view\"");
        assert_json_eq(&FeedbackContext::RequeriedAnswer, "\"requeried_answer\"");
        assert_json_eq(&FeedbackContext::SlowResult, "\"slow_result\"");
    }

    #[test]
    fn test_feedback_response_serialization() {
        assert_json_eq(&FeedbackResponse::Unclear, "\"unclear\"");
        assert_json_eq(&FeedbackResponse::MissingContext, "\"missing_context\"");
        assert_json_eq(&FeedbackResponse::TooSlow, "\"too_slow\"");
        assert_json_eq(&FeedbackResponse::NotUseful, "\"not_useful\"");
        assert_json_eq(&FeedbackResponse::Dismissed, "\"dismissed\"");
    }

    #[test]
    fn test_error_kind_serialization() {
        assert_json_eq(&ErrorKind::ParseError, "\"parse_error\"");
        assert_json_eq(&ErrorKind::IndexError, "\"index_error\"");
        assert_json_eq(&ErrorKind::QueryError, "\"query_error\"");
        assert_json_eq(&ErrorKind::IoError, "\"io_error\"");
        assert_json_eq(&ErrorKind::ConfigError, "\"config_error\"");
        assert_json_eq(&ErrorKind::PluginError, "\"plugin_error\"");
        assert_json_eq(&ErrorKind::Unknown, "\"unknown\"");
    }

    #[test]
    fn test_error_category_serialization() {
        assert_json_eq(&ErrorCategory::User, "\"user\"");
        assert_json_eq(&ErrorCategory::System, "\"system\"");
        assert_json_eq(&ErrorCategory::Network, "\"network\"");
        assert_json_eq(&ErrorCategory::Internal, "\"internal\"");
    }

    #[test]
    fn test_os_kind_serialization() {
        assert_json_eq(&OsKind::Linux, "\"linux\"");
        assert_json_eq(&OsKind::MacOs, "\"mac_os\"");
        assert_json_eq(&OsKind::Windows, "\"windows\"");
        assert_json_eq(&OsKind::FreeBsd, "\"free_bsd\"");
        assert_json_eq(&OsKind::Other, "\"other\"");
    }

    #[test]
    fn test_arch_kind_serialization() {
        assert_json_eq(&ArchKind::X86_64, "\"x86_64\"");
        assert_json_eq(&ArchKind::Aarch64, "\"aarch64\"");
        assert_json_eq(&ArchKind::Arm, "\"arm\"");
        assert_json_eq(&ArchKind::Other, "\"other\"");
    }

    #[test]
    fn test_build_kind_serialization() {
        assert_json_eq(&BuildKind::Release, "\"release\"");
        assert_json_eq(&BuildKind::Debug, "\"debug\"");
        assert_json_eq(&BuildKind::Custom, "\"custom\"");
    }

    #[test]
    fn test_iso_week_period_validation() {
        // Valid formats
        assert!(IsoWeekPeriod::try_new("2025-W01").is_ok());
        assert!(IsoWeekPeriod::try_new("2025-W50").is_ok());
        assert!(IsoWeekPeriod::try_new("2025-W53").is_ok());

        // Invalid formats
        assert!(IsoWeekPeriod::try_new("invalid").is_err());
        assert!(IsoWeekPeriod::try_new("2025-W00").is_err()); // Week 0 invalid
        assert!(IsoWeekPeriod::try_new("2025-W54").is_err()); // Week 54 invalid
        assert!(IsoWeekPeriod::try_new("25-W50").is_err()); // Wrong year format
    }

    #[test]
    fn test_sqry_version_validation() {
        // Valid semver formats
        assert!(SqryVersion::try_new("0.5.0").is_ok());
        assert!(SqryVersion::try_new("1.0.0").is_ok());
        assert!(SqryVersion::try_new("1.2.3-beta.1").is_ok());

        // Invalid formats
        assert!(SqryVersion::try_new("not-semver").is_err());
        assert!(SqryVersion::try_new("1.2").is_err());
        assert!(SqryVersion::try_new("v1.0.0").is_err());
    }

    #[test]
    fn test_diagnostics_summary_defaults() {
        let summary = DiagnosticsSummary::default();

        assert!(summary.top_workflows.is_empty());
        assert_f64_close(summary.avg_time_to_result_sec, 0.0);
        assert_eq!(summary.total_uses, 0);
        assert_eq!(summary.dropped_events, 0);
    }

    #[test]
    fn test_share_snapshot_from_summary() {
        let summary = DiagnosticsSummary {
            period: IsoWeekPeriod::try_new("2025-W50").unwrap(),
            top_workflows: vec![TopWorkflow {
                kind: QueryKind::CallChain,
                count: 47,
            }],
            avg_time_to_result_sec: 1.2,
            median_time_to_result_sec: 1.0,
            abandon_rate: 0.12,
            abandonment: vec![GraphAbandonRate {
                kind: GraphKind::CallGraph,
                rate: 0.15,
            }],
            ai_requery_rate: 0.31,
            total_uses: 159,
            dropped_events: 0,
        };

        let snapshot = ShareSnapshot::from_summary(&summary);

        assert_eq!(snapshot.period.as_str(), "2025-W50");
        assert_eq!(snapshot.top_workflows.len(), 1);
        assert_eq!(snapshot.total_uses, 159);
    }

    #[test]
    fn test_no_paths_in_serialized_event() {
        let event = UseEvent::test_event();
        let json = serde_json::to_string(&event).unwrap();

        // Must not contain path-like strings
        assert!(!json.contains("/home/"));
        assert!(!json.contains("/Users/"));
        assert!(!json.contains("C:\\"));
        assert!(!json.contains("/srv/"));

        // Must not contain code-like strings
        assert!(!json.contains("fn "));
        assert!(!json.contains("class "));
        assert!(!json.contains("def "));
    }

    #[test]
    fn test_top_workflow_serialization() {
        let workflow = TopWorkflow {
            kind: QueryKind::CallChain,
            count: 47,
        };

        let json = serde_json::to_string(&workflow).unwrap();

        // Should serialize as object with kind/count
        assert!(json.contains(r#""kind":"call_chain""#));
        assert!(json.contains(r#""count":47"#));
    }

    #[test]
    fn test_workflow_step_tagged_serialization() {
        let step = WorkflowStep::QueryStarted {
            kind: QueryKind::ImpactAnalysis,
        };

        let json = serde_json::to_string(&step).unwrap();

        // Should use internally tagged format
        assert!(json.contains(r#""type":"query_started""#));
        assert!(json.contains(r#""kind":"impact_analysis""#));
    }

    #[test]
    fn test_system_info_current() {
        let info = SystemInfo::current();

        // Should detect something (not panic)
        let _ = info.os;
        let _ = info.arch;
        assert!(!info.sqry_version.as_str().is_empty());
    }
}
