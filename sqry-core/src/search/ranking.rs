//! Result ranking and relevance scoring for hybrid search
//!
//! This module implements algorithms for ranking and scoring search results
//! to present the most relevant matches first.
//!
//! NOTE: Ranking methods that depended on the legacy index have been removed.
//! Use CodeGraph-based ranking in the query executor instead.

use super::Match as TextMatch;
use crate::search::simd;
use std::cmp::Ordering;
use std::path::Path;

/// Scoring weights for different relevance factors
#[derive(Debug, Clone)]
pub struct RankingWeights {
    /// Weight for exact name matches (default: 10.0)
    pub exact_name_match: f64,
    /// Weight for partial name matches (default: 5.0)
    pub partial_name_match: f64,
    /// Weight for file name matches (default: 3.0)
    pub file_name_match: f64,
    /// Weight for text match position (earlier = higher, default: 2.0)
    pub position_weight: f64,
    /// Weight for symbol type priority (default: 1.0)
    pub symbol_type_weight: f64,
    /// Penalty for deep directory nesting (default: 0.5)
    pub depth_penalty: f64,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            exact_name_match: 10.0,
            partial_name_match: 5.0,
            file_name_match: 3.0,
            position_weight: 2.0,
            symbol_type_weight: 1.0,
            depth_penalty: 0.5,
        }
    }
}

/// A ranked result that can be either a symbol or text match
#[derive(Debug, Clone)]
pub enum RankedResult {
    /// A text match with its relevance score
    TextMatch {
        /// The text match from grep search
        text_match: TextMatch,
        /// The relevance score (higher = more relevant)
        score: f64,
        /// Human-readable reason for the score
        reason: String,
    },
}

impl RankedResult {
    /// Get the score of this result
    #[must_use]
    pub fn score(&self) -> f64 {
        match self {
            RankedResult::TextMatch { score, .. } => *score,
        }
    }

    /// Get the file path of this result
    #[must_use]
    pub fn file_path(&self) -> &Path {
        match self {
            RankedResult::TextMatch { text_match, .. } => text_match.path.as_path(),
        }
    }

    /// Get the reason for the score
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            RankedResult::TextMatch { reason, .. } => reason,
        }
    }
}

/// Result ranker for hybrid search
pub struct ResultRanker {
    weights: RankingWeights,
}

impl ResultRanker {
    /// Create a new result ranker with default weights
    #[must_use]
    pub fn new() -> Self {
        Self {
            weights: RankingWeights::default(),
        }
    }

    /// Create a result ranker with custom weights
    #[must_use]
    pub fn with_weights(weights: RankingWeights) -> Self {
        Self { weights }
    }

    /// Rank text matches by relevance to the query
    #[must_use]
    pub fn rank_text_matches(&self, matches: Vec<TextMatch>, query: &str) -> Vec<RankedResult> {
        let query_lower = query.to_lowercase();

        let mut ranked: Vec<RankedResult> = matches
            .into_iter()
            .map(|text_match| {
                let (score, reason) = self.score_text_match(&text_match, &query_lower);
                RankedResult::TextMatch {
                    text_match,
                    score,
                    reason,
                }
            })
            .collect();

        // Sort by score (highest first)
        ranked.sort_by(|a, b| b.score().partial_cmp(&a.score()).unwrap_or(Ordering::Equal));

        ranked
    }

    /// Converts usize to f64, centralizing a potentially lossy cast.
    #[inline]
    #[allow(clippy::cast_precision_loss)] // Depths and counts stay well below 2^53; lossy cast is acceptable
    fn to_f64(n: usize) -> f64 {
        n as f64
    }

    fn lower_ascii_or_unicode(value: &str) -> String {
        if value.is_ascii() {
            simd::to_lowercase_ascii(value)
        } else {
            value.to_lowercase()
        }
    }

    fn apply_depth_penalty(&self, depth: usize, reasons: &mut Vec<String>) -> f64 {
        if depth > 3 {
            let penalty = Self::to_f64(depth - 3) * self.weights.depth_penalty;
            if penalty > 1.0 {
                reasons.push(format!("depth penalty: {depth} levels"));
            }
            return -penalty;
        }

        0.0
    }

    fn boost_code_file(file_path: &Path, reasons: &mut Vec<String>) -> f64 {
        let is_code_file = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                matches!(ext.to_ascii_lowercase().as_str(), "rs" | "py" | "ts" | "js")
            });

        if is_code_file {
            reasons.push("code file".to_string());
            return 1.0;
        }

        0.0
    }

    fn is_comment_line(line: &str) -> bool {
        line.starts_with("//") || line.starts_with("/*") || line.starts_with('#')
    }

    /// Score a text match's relevance to the query
    fn score_text_match(&self, text_match: &TextMatch, query: &str) -> (f64, String) {
        let mut score = 5.0; // Base score for text matches
        let mut reasons = Vec::new();

        let file_path = &text_match.path;

        // SIMD-accelerated lowercase for code lines (typically ASCII)
        let line_lower = Self::lower_ascii_or_unicode(&text_match.line_text);
        let trimmed_line = line_lower.trim_start();

        // SIMD-accelerated lowercase for file names (ASCII paths)
        // Count occurrences in line
        let occurrences = line_lower.matches(query).count();
        if occurrences > 1 {
            score += Self::to_f64(occurrences) * 2.0;
            reasons.push(format!("{occurrences} occurrences"));
        }

        // Check if match is in a comment (lower priority)
        if Self::is_comment_line(trimmed_line) {
            score -= 1.0;
            reasons.push("comment match".to_string());
        }

        // Boost matches in important file types
        score += Self::boost_code_file(file_path, &mut reasons);

        // Position weighting (earlier lines = higher score)
        let position_score =
            (1000.0 - f64::from(text_match.line.min(1000))) / 1000.0 * self.weights.position_weight;
        score += position_score;
        if text_match.line < 100 {
            reasons.push(format!("early in file (line {})", text_match.line));
        }

        // Penalize deep directory nesting
        score += self.apply_depth_penalty(text_match.path.components().count(), &mut reasons);

        let reason = if reasons.is_empty() {
            format!("text match at line {}", text_match.line)
        } else {
            reasons.join(", ")
        };

        (score.max(0.0), reason)
    }
}

impl Default for ResultRanker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn create_test_text_match(path: &str, line: u32, text: &str) -> TextMatch {
        TextMatch {
            path: PathBuf::from(path),
            line,
            line_text: text.to_string(),
            byte_offset: 0,
        }
    }

    #[test]
    fn test_text_match_early_line_scores_higher() {
        let ranker = ResultRanker::new();
        let matches = vec![
            create_test_text_match("src/lib.rs", 500, "TODO: fix this"),
            create_test_text_match("src/lib.rs", 10, "TODO: implement"),
        ];

        let ranked_results = ranker.rank_text_matches(matches, "TODO");

        // Line 10 should score higher than line 500
        let RankedResult::TextMatch { text_match, .. } = &ranked_results[0];
        assert_eq!(text_match.line, 10);
    }

    #[test]
    fn test_multiple_occurrences_boost_score() {
        let ranker = ResultRanker::new();
        let matches = vec![
            create_test_text_match("src/lib.rs", 10, "TODO: fix TODO TODO"),
            create_test_text_match("src/lib.rs", 11, "TODO: implement"),
        ];

        let ranked_results = ranker.rank_text_matches(matches, "TODO");

        // Multiple occurrences should score higher
        let RankedResult::TextMatch { text_match, .. } = &ranked_results[0];
        assert_eq!(text_match.line, 10); // 3 occurrences
    }
}
