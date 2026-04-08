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
#[allow(clippy::similar_names)] // Test variables: ranker/ranked, custom_ranker/custom_ranked are intentional
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

    #[test]
    fn test_result_ranker_default() {
        let ranker = ResultRanker::default();
        let matches = vec![create_test_text_match("src/lib.rs", 1, "hello world")];
        let ranked = ranker.rank_text_matches(matches, "hello");
        assert_eq!(ranked.len(), 1);
    }

    #[test]
    fn test_result_ranker_with_custom_weights() {
        // Custom position_weight (4.0) is double the default (2.0).  For an
        // early-line match in a code file the position component dominates, so
        // the custom-weights score must be strictly higher than the default.
        let weights = RankingWeights {
            exact_name_match: 20.0,
            partial_name_match: 10.0,
            file_name_match: 5.0,
            position_weight: 4.0,
            symbol_type_weight: 2.0,
            depth_penalty: 1.0,
        };
        let custom_ranker = ResultRanker::with_weights(weights);
        let default_ranker = ResultRanker::new();

        let input = vec![create_test_text_match("src/lib.rs", 1, "hello")];
        let custom_ranked = custom_ranker.rank_text_matches(input.clone(), "hello");
        let default_ranked = default_ranker.rank_text_matches(input, "hello");

        assert_eq!(custom_ranked.len(), 1);
        assert!(
            custom_ranked[0].score() > default_ranked[0].score(),
            "custom position_weight=4.0 should produce a higher score than default 2.0: \
             custom={:.3}, default={:.3}",
            custom_ranked[0].score(),
            default_ranked[0].score()
        );
    }

    #[test]
    fn test_ranked_result_file_path() {
        let ranker = ResultRanker::new();
        let matches = vec![create_test_text_match("src/main.rs", 5, "fn main() {}")];
        let ranked = ranker.rank_text_matches(matches, "main");
        let path = ranked[0].file_path();
        assert_eq!(path, std::path::Path::new("src/main.rs"));
    }

    #[test]
    fn test_ranked_result_reason_non_empty() {
        let ranker = ResultRanker::new();
        // Line < 100 → "early in file" reason
        let matches = vec![create_test_text_match("src/lib.rs", 1, "fn foo() {}")];
        let ranked = ranker.rank_text_matches(matches, "foo");
        let reason = ranked[0].reason();
        assert!(!reason.is_empty());
        assert!(reason.contains("early in file") || reason.contains("text match"));
    }

    #[test]
    fn test_ranked_result_reason_no_reasons_fallback() {
        // Line >= 100, not a code file ext, no comment, single occurrence
        let ranker = ResultRanker::new();
        let matches = vec![create_test_text_match(
            "notes.txt",
            200,
            "some foo text here",
        )];
        let ranked = ranker.rank_text_matches(matches, "foo");
        let reason = ranked[0].reason();
        // When no specific reasons, falls back to "text match at line N"
        assert!(reason.contains("text match at line 200"));
    }

    #[test]
    fn test_comment_line_slash_slash_lowers_score() {
        let ranker = ResultRanker::new();
        let comment_match = create_test_text_match("src/lib.rs", 5, "// foo comment");
        let code_match = create_test_text_match("src/lib.rs", 6, "let foo = 1;");
        let ranked = ranker.rank_text_matches(vec![comment_match, code_match], "foo");
        // Code match should outrank comment match
        let RankedResult::TextMatch {
            text_match, reason, ..
        } = &ranked[0];
        assert_ne!(
            text_match.line, 5,
            "comment line should not be first; reason: {reason}"
        );
    }

    #[test]
    fn test_comment_line_slash_star() {
        let ranker = ResultRanker::new();
        // Line starting with /* should be penalized
        let comment_match = create_test_text_match("src/lib.rs", 5, "/* foo block */");
        let code_match = create_test_text_match("src/lib.rs", 6, "fn foo() {}");
        let ranked = ranker.rank_text_matches(vec![comment_match, code_match], "foo");
        let RankedResult::TextMatch { text_match, .. } = &ranked[0];
        assert_eq!(text_match.line, 6, "code line should outrank block comment");
    }

    #[test]
    fn test_comment_line_hash() {
        let ranker = ResultRanker::new();
        // Line starting with # should be penalized
        let comment_match = create_test_text_match("script.py", 5, "# foo comment");
        let code_match = create_test_text_match("script.py", 6, "def foo():");
        let ranked = ranker.rank_text_matches(vec![comment_match, code_match], "foo");
        let RankedResult::TextMatch { text_match, .. } = &ranked[0];
        assert_eq!(text_match.line, 6, "code line should outrank hash comment");
    }

    #[test]
    fn test_depth_penalty_over_threshold_large_penalty() {
        let ranker = ResultRanker::new();
        // Depth > 3 with many components → penalty > 1.0 → reason added
        // "a/b/c/d/e/f/g/file.rs" has 8 components → depth=8, penalty=(8-3)*0.5=2.5 > 1.0
        let deep_match = create_test_text_match("a/b/c/d/e/f/g/file.rs", 1, "foo search");
        let ranked = ranker.rank_text_matches(vec![deep_match], "foo");
        let reason = ranked[0].reason();
        assert!(
            reason.contains("depth penalty"),
            "expected depth penalty in reason, got: {reason}"
        );
    }

    #[test]
    fn test_code_file_extensions_get_boost() {
        let ranker = ResultRanker::new();
        // .rs, .py, .ts, .js should all get code file boost
        for ext in &["rs", "py", "ts", "js"] {
            let path = format!("src/file.{ext}");
            let code_match = create_test_text_match(&path, 200, "foo bar");
            let txt_match = create_test_text_match("notes.txt", 200, "foo bar");
            let ranked = ranker.rank_text_matches(vec![code_match, txt_match], "foo");
            let RankedResult::TextMatch {
                text_match, reason, ..
            } = &ranked[0];
            assert!(
                text_match.path.to_str().unwrap().ends_with(ext),
                ".{ext} file should outscore .txt; reason: {reason}"
            );
        }
    }

    #[test]
    fn test_non_code_file_no_boost() {
        let ranker = ResultRanker::new();
        let txt_match = create_test_text_match("readme.md", 1, "foo text");
        let ranked = ranker.rank_text_matches(vec![txt_match], "foo");
        let reason = ranked[0].reason();
        // Should not contain "code file" in reason
        assert!(
            !reason.contains("code file"),
            "md file should not get code file boost: {reason}"
        );
    }

    #[test]
    fn test_unicode_line_text_does_not_panic() {
        // Non-ASCII lines should use Unicode lowercasing (not SIMD)
        let ranker = ResultRanker::new();
        let matches = vec![create_test_text_match(
            "src/lib.rs",
            5,
            "fn café() { /* unicode: αβγ */ }",
        )];
        let ranked = ranker.rank_text_matches(matches, "café");
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].score() > 0.0);
    }

    #[test]
    fn test_line_at_or_above_100_no_early_in_file_reason() {
        let ranker = ResultRanker::new();
        let matches = vec![create_test_text_match("notes.txt", 100, "foo match here")];
        let ranked = ranker.rank_text_matches(matches, "foo");
        let reason = ranked[0].reason();
        assert!(
            !reason.contains("early in file"),
            "line 100 should not be 'early in file': {reason}"
        );
    }

    #[test]
    fn test_empty_matches_returns_empty() {
        let ranker = ResultRanker::new();
        let ranked = ranker.rank_text_matches(vec![], "query");
        assert!(ranked.is_empty());
    }

    #[test]
    fn test_ranked_result_score() {
        // "src/lib.rs" line 1 with query "foo" matching "fn foo() {}":
        //   base=5.0, code_file=+1.0, position=(999/1000)*2.0≈1.998
        //   depth=2 components (no penalty ≤3)
        //   → total ≈ 7.998
        // We validate the score is in the ballpark [7.0, 10.0] to catch
        // regressions without being fragile to minor float changes.
        let ranker = ResultRanker::new();
        let matches = vec![create_test_text_match("src/lib.rs", 1, "fn foo() {}")];
        let ranked = ranker.rank_text_matches(matches, "foo");
        let score = ranked[0].score();
        assert!(score > 0.0, "score must be positive, got {score}");
        assert!(
            (7.0..=10.0).contains(&score),
            "score {score:.3} is outside the expected range [7.0, 10.0] for a line-1 code-file match"
        );
    }
}
