//! Fuzzy string matching for identifier search.
//!
//! This module provides fuzzy matching capabilities using various algorithms
//! (Levenshtein distance, Jaro-Winkler) to find identifiers that approximately
//! match a query string.

use crate::search::simd;
use rapidfuzz::distance::{jaro_winkler, levenshtein};

/// Algorithm to use for fuzzy matching
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchAlgorithm {
    /// Levenshtein edit distance (measures insertions, deletions, substitutions)
    Levenshtein,
    /// Jaro-Winkler similarity (good for typos near the beginning)
    JaroWinkler,
}

impl Default for MatchAlgorithm {
    fn default() -> Self {
        Self::JaroWinkler
    }
}

/// Configuration for fuzzy matching
#[derive(Debug, Clone)]
pub struct MatchConfig {
    /// Algorithm to use for matching
    pub algorithm: MatchAlgorithm,

    /// Minimum similarity score (0.0-1.0) to consider a match
    /// For Levenshtein: 1.0 - (distance / `max_len`)
    /// For Jaro-Winkler: direct similarity score
    pub min_score: f64,

    /// Case-sensitive matching
    pub case_sensitive: bool,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            algorithm: MatchAlgorithm::JaroWinkler,
            min_score: 0.6,
            case_sensitive: false,
        }
    }
}

/// Result of a fuzzy match
#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    /// Similarity score (0.0-1.0, higher is better)
    pub score: f64,
    /// Index of the matched entry
    pub entry_id: usize,
}

impl MatchResult {
    /// Create a new match result
    #[must_use]
    pub fn new(score: f64, entry_id: usize) -> Self {
        Self { score, entry_id }
    }
}

/// Fuzzy matcher for identifier names
pub struct FuzzyMatcher {
    config: MatchConfig,
}

impl FuzzyMatcher {
    /// Create a new fuzzy matcher with default configuration
    #[must_use]
    pub fn new() -> Self {
        Self {
            config: MatchConfig::default(),
        }
    }

    /// Create a new fuzzy matcher with custom configuration
    #[must_use]
    pub fn with_config(config: MatchConfig) -> Self {
        Self { config }
    }

    /// Calculate similarity score between query and target
    ///
    /// Returns a score between 0.0 and 1.0 (1.0 = perfect match)
    ///
    /// # Performance
    /// Uses SIMD-accelerated lowercase conversion for ASCII-only targets (identifier names).
    /// Query normalization uses stdlib to handle Unicode user input correctly.
    #[must_use]
    pub fn score(&self, query: &str, target: &str) -> f64 {
        let (query_normalized, target_normalized) = if self.config.case_sensitive {
            (query.to_string(), target.to_string())
        } else {
            // Query: user input, may contain Unicode, use stdlib
            let query_norm = query.to_lowercase();

            // Target: identifier name, typically ASCII, use SIMD
            let target_norm = if target.is_ascii() {
                simd::to_lowercase_ascii(target)
            } else {
                target.to_lowercase()
            };

            (query_norm, target_norm)
        };

        match self.config.algorithm {
            MatchAlgorithm::Levenshtein => {
                self.levenshtein_similarity(&query_normalized, &target_normalized)
            }
            MatchAlgorithm::JaroWinkler => {
                jaro_winkler::similarity(query_normalized.chars(), target_normalized.chars())
            }
        }
    }

    /// Calculate Levenshtein similarity (normalized to 0.0-1.0)
    #[allow(clippy::cast_precision_loss, clippy::unused_self)]
    fn levenshtein_similarity(&self, query: &str, target: &str) -> f64 {
        let distance = levenshtein::distance(query.chars(), target.chars());

        // Use character count (not byte length) for proper Unicode normalization
        let max_len = query.chars().count().max(target.chars().count()) as f64;

        if max_len == 0.0 {
            return 1.0; // Both strings empty
        }

        1.0 - (distance as f64 / max_len)
    }

    /// Match a query against a single target
    ///
    /// Returns Some(score) if the match exceeds `min_score` threshold, None otherwise
    #[must_use]
    pub fn match_one(&self, query: &str, target: &str) -> Option<f64> {
        let score = self.score(query, target);

        if score >= self.config.min_score {
            Some(score)
        } else {
            None
        }
    }

    /// Match a query against multiple targets with their entry IDs
    ///
    /// Returns a Vec of `MatchResult` sorted by score (descending)
    ///
    /// # Arguments
    ///
    /// * `query` - The query string to match
    /// * `targets` - Iterator of (`entry_id`, `target_name`) tuples
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::search::matcher::{FuzzyMatcher, MatchConfig};
    ///
    /// let matcher = FuzzyMatcher::new();
    /// let targets = vec![
    ///     (0, "hello_world"),
    ///     (1, "helo_world"),
    ///     (2, "goodbye"),
    /// ];
    ///
    /// let matches = matcher.match_many("hello", targets.into_iter());
    /// assert_eq!(matches[0].entry_id, 0); // "hello_world" is best match
    /// assert!(matches[0].score > matches[1].score);
    /// ```
    pub fn match_many<'a, I>(&self, query: &str, targets: I) -> Vec<MatchResult>
    where
        I: Iterator<Item = (usize, &'a str)>,
    {
        let mut results: Vec<MatchResult> = targets
            .filter_map(|(entry_id, target)| {
                self.match_one(query, target)
                    .map(|score| MatchResult::new(score, entry_id))
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results
    }

    /// Get the current configuration
    #[must_use]
    pub fn config(&self) -> &MatchConfig {
        &self.config
    }
}

impl Default for FuzzyMatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn test_levenshtein_exact_match() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::Levenshtein,
            min_score: 0.6,
            case_sensitive: false,
        };
        let matcher = FuzzyMatcher::with_config(config);

        let score = matcher.score("hello", "hello");
        assert_abs_diff_eq!(score, 1.0, epsilon = 1e-10);
    }

    #[test]
    fn test_levenshtein_similar() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::Levenshtein,
            min_score: 0.6,
            case_sensitive: false,
        };
        let matcher = FuzzyMatcher::with_config(config);

        let score = matcher.score("hello", "helo");
        assert!(score > 0.7); // Should be similar (1 deletion)
    }

    #[test]
    fn test_jaro_winkler_exact_match() {
        let matcher = FuzzyMatcher::new(); // Uses Jaro-Winkler by default
        let score = matcher.score("hello", "hello");
        assert_abs_diff_eq!(score, 1.0, epsilon = 1e-10);
    }

    #[test]
    fn test_jaro_winkler_similar() {
        let matcher = FuzzyMatcher::new();
        let score = matcher.score("hello", "helo");
        assert!(score > 0.8); // Jaro-Winkler is good for typos
    }

    #[test]
    fn test_case_insensitive() {
        let matcher = FuzzyMatcher::new();
        let score = matcher.score("HELLO", "hello");
        assert_abs_diff_eq!(score, 1.0, epsilon = 1e-10);
    }

    #[test]
    fn test_case_sensitive() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::JaroWinkler,
            min_score: 0.6,
            case_sensitive: true,
        };
        let matcher = FuzzyMatcher::with_config(config);

        let score = matcher.score("HELLO", "hello");
        assert!(score < 1.0);
    }

    #[test]
    fn test_match_one_threshold() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::JaroWinkler,
            min_score: 0.8,
            case_sensitive: false,
        };
        let matcher = FuzzyMatcher::with_config(config);

        assert!(matcher.match_one("hello", "hello").is_some());
        assert!(matcher.match_one("hello", "helo").is_some());
        assert!(matcher.match_one("hello", "goodbye").is_none());
    }

    #[test]
    fn test_match_many_sorted() {
        let matcher = FuzzyMatcher::new();
        let targets = vec![
            (0, "goodbye"),
            (1, "helo_world"),
            (2, "hello_world"),
            (3, "hello"),
        ];

        let match_results = matcher.match_many("hello", targets.into_iter());

        // Should have matches, sorted by score
        assert!(!match_results.is_empty());
        assert_eq!(match_results[0].entry_id, 3); // "hello" is exact match
        assert!(match_results[0].score >= match_results[1].score);
    }

    #[test]
    fn test_match_many_filters_low_scores() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::JaroWinkler,
            min_score: 0.9,
            case_sensitive: false,
        };
        let matcher = FuzzyMatcher::with_config(config);

        let targets = vec![(0, "hello"), (1, "helo"), (2, "goodbye")];

        let match_results = matcher.match_many("hello", targets.into_iter());

        // Should only get high-scoring matches
        assert!(match_results.len() <= 2); // "hello" and maybe "helo"
        assert!(match_results.iter().all(|m| m.score >= 0.9));
    }

    #[test]
    fn test_empty_strings() {
        let matcher = FuzzyMatcher::new();
        let score = matcher.score("", "");
        assert_abs_diff_eq!(score, 1.0, epsilon = 1e-10);
    }

    #[test]
    fn test_levenshtein_similarity_calculation() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::Levenshtein,
            min_score: 0.6,
            case_sensitive: false,
        };
        let matcher = FuzzyMatcher::with_config(config);

        // "kitten" -> "sitting" = 3 edits, max_len = 7 chars
        // similarity = 1.0 - (3/7) ≈ 0.571
        let score = matcher.score("kitten", "sitting");
        assert!((score - 0.571).abs() < 0.01);
    }

    #[test]
    fn test_levenshtein_unicode_normalization() {
        let config = MatchConfig {
            algorithm: MatchAlgorithm::Levenshtein,
            min_score: 0.0,
            case_sensitive: false,
        };
        let matcher = FuzzyMatcher::with_config(config);

        // Unicode test: "café" (4 chars, 5 bytes) vs "cafe" (4 chars, 4 bytes)
        // Distance = 1 (substitution: é -> e)
        // max_len = 4 chars (not 5 bytes)
        // similarity = 1.0 - (1/4) = 0.75
        let score = matcher.score("café", "cafe");
        assert!((score - 0.75).abs() < 0.01);

        // Another test: "hello世界" (7 chars) vs "hello" (5 chars)
        // Distance = 2 (delete 世, delete 界)
        // max_len = 7 chars
        // similarity = 1.0 - (2/7) ≈ 0.714
        let score = matcher.score("hello世界", "hello");
        assert!((score - 0.714).abs() < 0.02);
    }
}
