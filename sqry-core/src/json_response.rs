//! Structured JSON response types for LLM tool integration
//!
//! This module provides standardized response wrappers that include query metadata,
//! statistics, and results in a consistent format across all sqry commands.

use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;

use crate::confidence::ConfidenceMetadata;

/// Query metadata included in all responses
#[derive(Debug, Clone, Serialize)]
pub struct QueryMeta {
    /// The search pattern or query expression
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,

    /// Active filters applied to the query
    #[serde(skip_serializing_if = "Filters::is_empty")]
    pub filters: Filters,

    /// Query execution time in milliseconds
    pub execution_time_ms: f64,

    /// Optional query execution plan (when explain=true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,

    /// Confidence metadata for the analysis (Rust-specific)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<ConfidenceMetadata>,
}

impl QueryMeta {
    /// Create new query metadata
    #[must_use]
    pub fn new(pattern: Option<String>, execution_time: Duration) -> Self {
        Self {
            pattern,
            filters: Filters::default(),
            execution_time_ms: execution_time.as_secs_f64() * 1000.0,
            plan: None,
            confidence: None,
        }
    }

    /// Set filters
    #[must_use]
    pub fn with_filters(mut self, filters: Filters) -> Self {
        self.filters = filters;
        self
    }

    /// Set execution plan
    #[must_use]
    pub fn with_plan(mut self, plan: String) -> Self {
        self.plan = Some(plan);
        self
    }

    /// Set confidence metadata
    #[must_use]
    pub fn with_confidence(mut self, confidence: ConfidenceMetadata) -> Self {
        self.confidence = Some(confidence);
        self
    }
}

/// Filters applied to a query
#[derive(Debug, Clone, Default, Serialize)]
pub struct Filters {
    /// Node kind filter (function, class, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    /// Language filter
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,

    /// Case-insensitive flag
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub ignore_case: bool,

    /// Exact match flag
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub exact: bool,

    /// Fuzzy search settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fuzzy: Option<FuzzyFilters>,
}

impl Filters {
    /// Check if filters are empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kind.is_none()
            && self.lang.is_none()
            && !self.ignore_case
            && !self.exact
            && self.fuzzy.is_none()
    }
}

/// Fuzzy search specific filters
#[derive(Debug, Clone, Serialize)]
pub struct FuzzyFilters {
    /// Fuzzy matching algorithm
    pub algorithm: String,

    /// Minimum similarity threshold (0.0-1.0)
    pub threshold: f64,

    /// Maximum candidates to consider
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_candidates: Option<usize>,
}

/// Statistics about the query results
#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    /// Total number of matches found
    pub total_matches: usize,

    /// Number of results returned (after limiting)
    pub returned: usize,

    /// Whether results were truncated due to limit
    #[serde(rename = "truncated")]
    pub is_truncated: bool,

    /// Age of the index in seconds (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_age_seconds: Option<u64>,

    /// Number of candidates generated (for fuzzy search)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate_count: Option<usize>,

    /// Number of candidates after filtering (for fuzzy search)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filtered_count: Option<usize>,

    /// True if the index was found in an ancestor directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub used_ancestor_index: Option<bool>,

    /// Scope filter applied (relative path like "src/**" or "main.rs")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filtered_to: Option<String>,
}

impl Stats {
    /// Create new stats with basic counts
    #[must_use]
    pub fn new(total: usize, returned: usize) -> Self {
        Self {
            total_matches: total,
            returned,
            is_truncated: returned < total,
            index_age_seconds: None,
            candidate_count: None,
            filtered_count: None,
            used_ancestor_index: None,
            filtered_to: None,
        }
    }

    /// Set index age
    #[must_use]
    pub fn with_index_age(mut self, age_seconds: u64) -> Self {
        self.index_age_seconds = Some(age_seconds);
        self
    }

    /// Set candidate counts (for fuzzy search)
    #[must_use]
    pub fn with_candidates(mut self, total: usize, filtered: usize) -> Self {
        self.candidate_count = Some(total);
        self.filtered_count = Some(filtered);
        self
    }

    /// Set scope info when using ancestor index discovery
    #[must_use]
    pub fn with_scope_info(mut self, is_ancestor: bool, filtered_to: Option<String>) -> Self {
        self.used_ancestor_index = Some(is_ancestor);
        self.filtered_to = filtered_to;
        self
    }
}

/// Structured JSON response wrapper
#[derive(Debug, Clone, Serialize)]
pub struct JsonResponse<T> {
    /// Query metadata
    pub query: QueryMeta,

    /// Result statistics
    pub stats: Stats,

    /// The actual results
    pub results: Vec<T>,
}

impl<T> JsonResponse<T> {
    /// Create a new JSON response
    #[must_use]
    pub fn new(query: QueryMeta, stats: Stats, results: Vec<T>) -> Self {
        Self {
            query,
            stats,
            results,
        }
    }
}

/// Streaming event for incremental results
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent<T> {
    /// Partial result with score
    PartialResult {
        /// The result item
        result: T,
        /// Match score (0.0-1.0)
        score: f64,
    },

    /// Final summary with complete statistics
    FinalSummary {
        /// Final statistics
        stats: Stats,
    },
}

/// Index status information for LLM agents
#[derive(Debug, Clone, Serialize)]
pub struct IndexStatus {
    /// Whether an index exists
    pub exists: bool,

    /// Path to the index directory
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,

    /// When the index was created (ISO 8601 format)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,

    /// Age of the index in seconds
    #[serde(skip_serializing_if = "Option::is_none")]
    pub age_seconds: Option<u64>,

    /// Total number of symbols indexed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_count: Option<usize>,

    /// Number of files indexed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,

    /// Languages found in the index
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<String>>,

    /// Whether fuzzy search is supported
    pub supports_fuzzy: bool,

    /// Whether relation queries are supported
    pub supports_relations: bool,

    /// Total count of cross-language relations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cross_language_relation_count: Option<usize>,

    /// Node counts grouped by kind (e.g., {"function": 25669, "class": 332})
    /// Used for tree view grouping in `VSCode` extension
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_counts_by_kind: Option<HashMap<String, usize>>,

    /// File counts grouped by language (e.g., {"rust": 1245, "javascript": 523})
    /// Used for tree view grouping in `VSCode` extension
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_counts_by_language: Option<HashMap<String, usize>>,

    /// Relation counts grouped by language pair (e.g., {"go→javascript": 45})
    /// Used for tree view grouping in `VSCode` extension
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relation_counts_by_pair: Option<HashMap<String, usize>>,

    /// Whether index is considered stale (>24 hours old)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,

    /// Whether an index build is currently in progress (lock file exists)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub building: Option<bool>,

    /// Age of the lock file in seconds (if build is in progress)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_age_seconds: Option<u64>,
}

impl IndexStatus {
    /// Create a new index status for non-existent index
    #[must_use]
    pub fn not_found() -> Self {
        Self {
            exists: false,
            path: None,
            created_at: None,
            age_seconds: None,
            symbol_count: None,
            file_count: None,
            languages: None,
            supports_fuzzy: false,
            supports_relations: false,
            cross_language_relation_count: None,
            symbol_counts_by_kind: None,
            file_counts_by_language: None,
            relation_counts_by_pair: None,
            stale: None,
            building: None,
            build_age_seconds: None,
        }
    }

    /// Create index status from metadata (builder pattern to avoid too many args)
    #[must_use]
    pub fn from_index(path: String, created_at: String, age_seconds: u64) -> IndexStatusBuilder {
        IndexStatusBuilder {
            path,
            created_at,
            age_seconds,
            symbol_count: 0,
            file_count: None,
            languages: Vec::new(),
            has_relations: false,
            has_trigram: false,
            cross_language_relation_count: 0,
            symbol_counts_by_kind: None,
            file_counts_by_language: None,
            relation_counts_by_pair: None,
            building: None,
            build_age_seconds: None,
        }
    }
}

/// Builder for `IndexStatus` to avoid too many arguments
pub struct IndexStatusBuilder {
    path: String,
    created_at: String,
    age_seconds: u64,
    symbol_count: usize,
    file_count: Option<usize>,
    languages: Vec<String>,
    has_relations: bool,
    has_trigram: bool,
    cross_language_relation_count: usize,
    symbol_counts_by_kind: Option<HashMap<String, usize>>,
    file_counts_by_language: Option<HashMap<String, usize>>,
    relation_counts_by_pair: Option<HashMap<String, usize>>,
    building: Option<bool>,
    build_age_seconds: Option<u64>,
}

impl IndexStatusBuilder {
    /// Set the symbol count
    #[must_use]
    pub fn symbol_count(mut self, count: usize) -> Self {
        self.symbol_count = count;
        self
    }

    /// Set the file count
    #[must_use]
    pub fn file_count(mut self, count: usize) -> Self {
        self.file_count = Some(count);
        self
    }

    /// Set the file count as optional (None when unknown)
    #[must_use]
    pub fn file_count_opt(mut self, count: Option<usize>) -> Self {
        self.file_count = count;
        self
    }

    /// Set the languages list
    #[must_use]
    pub fn languages(mut self, langs: Vec<String>) -> Self {
        self.languages = langs;
        self
    }

    /// Set whether relations are present
    #[must_use]
    pub fn has_relations(mut self, value: bool) -> Self {
        self.has_relations = value;
        self
    }

    /// Set whether trigram index exists (for fuzzy search support)
    #[must_use]
    pub fn has_trigram(mut self, value: bool) -> Self {
        self.has_trigram = value;
        self
    }

    /// Set the cross-language relation count
    #[must_use]
    pub fn cross_language_relation_count(mut self, count: usize) -> Self {
        self.cross_language_relation_count = count;
        self
    }

    /// Set symbol counts grouped by kind (for tree view grouping)
    #[must_use]
    pub fn symbol_counts_by_kind(mut self, counts: HashMap<String, usize>) -> Self {
        self.symbol_counts_by_kind = Some(counts);
        self
    }

    /// Set file counts grouped by language (for tree view grouping)
    #[must_use]
    pub fn file_counts_by_language(mut self, counts: HashMap<String, usize>) -> Self {
        self.file_counts_by_language = Some(counts);
        self
    }

    /// Set relation counts grouped by language pair (for tree view grouping)
    #[must_use]
    pub fn relation_counts_by_pair(mut self, counts: HashMap<String, usize>) -> Self {
        self.relation_counts_by_pair = Some(counts);
        self
    }

    /// Set whether a build is currently in progress
    #[must_use]
    pub fn building(mut self, value: bool) -> Self {
        self.building = Some(value);
        self
    }

    /// Set the age of the build lock in seconds
    #[must_use]
    pub fn build_age_seconds(mut self, value: u64) -> Self {
        self.build_age_seconds = Some(value);
        self
    }

    /// Build the `IndexStatus`
    #[must_use]
    pub fn build(self) -> IndexStatus {
        let stale = self.age_seconds > 86400; // >24 hours
        IndexStatus {
            exists: true,
            path: Some(self.path),
            created_at: Some(self.created_at),
            age_seconds: Some(self.age_seconds),
            symbol_count: Some(self.symbol_count),
            file_count: self.file_count,
            languages: Some(self.languages),
            supports_fuzzy: self.has_trigram,
            supports_relations: self.has_relations,
            cross_language_relation_count: if self.cross_language_relation_count > 0 {
                Some(self.cross_language_relation_count)
            } else {
                None
            },
            symbol_counts_by_kind: self.symbol_counts_by_kind,
            file_counts_by_language: self.file_counts_by_language,
            relation_counts_by_pair: self.relation_counts_by_pair,
            stale: Some(stale),
            building: self.building,
            build_age_seconds: self.build_age_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn test_query_meta_serialization() {
        let meta = QueryMeta::new(Some("handler".to_string()), Duration::from_millis(5));

        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"pattern\":\"handler\""));
        assert!(json.contains("\"execution_time_ms\":5"));
    }

    #[test]
    fn test_filters_empty() {
        let filters = Filters::default();
        assert!(filters.is_empty());

        let json = serde_json::to_value(&filters).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 0);
    }

    #[test]
    fn test_filters_with_values() {
        let filters = Filters {
            kind: Some("function".to_string()),
            lang: Some("rust".to_string()),
            ignore_case: true,
            ..Default::default()
        };

        assert!(!filters.is_empty());
        let json = serde_json::to_string(&filters).unwrap();
        assert!(json.contains("\"kind\":\"function\""));
        assert!(json.contains("\"lang\":\"rust\""));
        assert!(json.contains("\"ignore_case\":true"));
    }

    #[test]
    fn test_stats_truncation() {
        let stats = Stats::new(100, 50);
        assert_eq!(stats.total_matches, 100);
        assert_eq!(stats.returned, 50);
        assert!(stats.is_truncated);

        let stats_not_truncated = Stats::new(50, 50);
        assert!(!stats_not_truncated.is_truncated);
    }

    #[test]
    fn test_json_response() {
        let meta = QueryMeta::new(Some("test".to_string()), Duration::from_millis(10));
        let stats = Stats::new(5, 5);
        let results = vec!["result1", "result2"];

        let response = JsonResponse::new(meta, stats, results);

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"query\""));
        assert!(json.contains("\"stats\""));
        assert!(json.contains("\"results\""));
    }

    #[test]
    fn test_stream_event_partial() {
        let event = StreamEvent::PartialResult {
            result: "test_symbol",
            score: 0.95,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"partial_result\""));
        assert!(json.contains("\"score\":0.95"));
    }

    #[test]
    fn test_stream_event_final() {
        let stats = Stats::new(42, 20);
        // Specify type parameter since FinalSummary carries no `T`
        let event: StreamEvent<()> = StreamEvent::FinalSummary { stats };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"event\":\"final_summary\""));
        assert!(json.contains("\"total_matches\":42"));
    }

    #[test]
    fn test_query_meta_with_confidence() {
        use crate::confidence::{ConfidenceLevel, ConfidenceMetadata};

        let confidence = ConfidenceMetadata {
            level: ConfidenceLevel::AstOnly,
            limitations: vec!["Missing rust-analyzer".to_string()],
            unavailable_features: vec!["Type inference".to_string()],
        };

        let meta = QueryMeta::new(Some("test_pattern".to_string()), Duration::from_millis(10))
            .with_confidence(confidence.clone());

        assert!(meta.confidence.is_some());
        let meta_confidence = meta.confidence.unwrap();
        assert_eq!(meta_confidence.level, ConfidenceLevel::AstOnly);
        assert_eq!(meta_confidence.limitations.len(), 1);
        assert_eq!(meta_confidence.unavailable_features.len(), 1);
    }

    #[test]
    fn test_query_meta_confidence_serialization() {
        use crate::confidence::{ConfidenceLevel, ConfidenceMetadata};

        let confidence = ConfidenceMetadata {
            level: ConfidenceLevel::Partial,
            limitations: vec!["Limited accuracy".to_string()],
            unavailable_features: vec![],
        };

        let meta = QueryMeta::new(Some("search_pattern".to_string()), Duration::from_millis(5))
            .with_confidence(confidence);

        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"confidence\""));
        assert!(json.contains("\"partial\""));
        assert!(json.contains("\"limitations\""));
        assert!(json.contains("\"Limited accuracy\""));
    }

    #[test]
    fn test_query_meta_without_confidence() {
        let meta = QueryMeta::new(Some("test".to_string()), Duration::from_millis(5));

        assert!(meta.confidence.is_none());

        let json = serde_json::to_string(&meta).unwrap();
        // None confidence should be omitted due to skip_serializing_if
        assert!(!json.contains("\"confidence\""));
    }

    #[test]
    fn test_json_response_with_confidence() {
        use crate::confidence::{ConfidenceLevel, ConfidenceMetadata};

        let confidence = ConfidenceMetadata {
            level: ConfidenceLevel::Verified,
            limitations: vec![],
            unavailable_features: vec![],
        };

        let meta = QueryMeta::new(Some("pattern".to_string()), Duration::from_millis(8))
            .with_confidence(confidence);
        let stats = Stats::new(10, 10);
        let results = vec!["result1", "result2"];

        let response = JsonResponse::new(meta, stats, results);

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"confidence\""));
        assert!(json.contains("\"verified\""));
    }
}
