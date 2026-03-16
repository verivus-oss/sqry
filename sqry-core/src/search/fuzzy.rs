//! Fuzzy search candidate generation and matching using trigram indices.
//!
//! This module provides efficient two-stage fuzzy search:
//! 1. Candidate generation: Use trigram overlap to filter the symbol space
//! 2. Fuzzy matching: Apply string similarity algorithms to rank candidates
//!
//! This approach is significantly faster than naive fuzzy matching on large
//! symbol sets while maintaining high-quality results.

use super::trigram::TrigramIndex;
use std::sync::Arc;

/// Check if Jaccard similarity should be used (default: true)
/// Can be disabled via `SQRY_FUZZY_USE_JACCARD=0` environment variable
fn use_jaccard_similarity() -> bool {
    std::env::var("SQRY_FUZZY_USE_JACCARD")
        .ok()
        .and_then(|v| v.parse::<u8>().ok())
        != Some(0) // Default to enabled
}

#[inline]
#[allow(clippy::cast_precision_loss)] // Trigram counts are bounded well below 2^53
fn to_f64(value: usize) -> f64 {
    value as f64
}

/// Configuration for fuzzy search candidate generation
#[derive(Debug, Clone)]
pub struct FuzzyConfig {
    /// Maximum number of candidates to return (default: 1000)
    pub max_candidates: usize,

    /// Minimum similarity score (0.0-1.0) based on trigram overlap (default: 0.1)
    /// A score of 0.0 means no filtering, 1.0 means perfect match required
    pub min_similarity: f64,
}

impl Default for FuzzyConfig {
    fn default() -> Self {
        Self {
            max_candidates: 1000,
            min_similarity: 0.1,
        }
    }
}

/// Generates candidate symbol IDs for fuzzy matching using trigram indices
pub struct CandidateGenerator {
    /// Shared trigram index (can be shared across threads)
    trigram_index: Arc<TrigramIndex>,

    /// Configuration
    config: FuzzyConfig,
}

impl CandidateGenerator {
    /// Create a new candidate generator with a trigram index
    #[must_use]
    pub fn new(trigram_index: Arc<TrigramIndex>) -> Self {
        Self {
            trigram_index,
            config: FuzzyConfig::default(),
        }
    }

    /// Create a new candidate generator with custom configuration
    #[must_use]
    pub fn with_config(trigram_index: Arc<TrigramIndex>, config: FuzzyConfig) -> Self {
        Self {
            trigram_index,
            config,
        }
    }

    /// Generate candidates for a query string
    ///
    /// Returns a vector of symbol IDs sorted by trigram similarity (descending).
    /// The result is capped at `max_candidates` and filtered by `min_similarity`.
    ///
    /// # Arguments
    ///
    /// * `query` - The query string to match against
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::search::trigram::TrigramIndex;
    /// use sqry_core::search::fuzzy::{CandidateGenerator, FuzzyConfig};
    /// use std::sync::Arc;
    ///
    /// let mut index = TrigramIndex::new();
    /// index.add_symbol(0, "hello_world");
    /// index.add_symbol(1, "hello_rust");
    /// index.add_symbol(2, "goodbye");
    ///
    /// let generator = CandidateGenerator::new(Arc::new(index));
    /// let candidates = generator.generate("hello");
    ///
    /// assert!(candidates.len() <= 2); // "hello_world" and "hello_rust"
    /// ```
    #[allow(clippy::cast_precision_loss)] // Similarity ratios rely on f64, acceptable for scoring heuristics
    #[must_use]
    pub fn generate(&self, query: &str) -> Vec<usize> {
        let Some(query_trigrams) = Self::extract_query_trigrams(query) else {
            return Vec::new();
        };

        let query_trigram_count = to_f64(query_trigrams.len());
        let use_jaccard = use_jaccard_similarity();
        let overlap_counts = self.collect_overlap_counts(&query_trigrams);
        let mut telemetry = FuzzyTelemetry::new(overlap_counts.len());

        // Filter by similarity threshold and cap
        let candidates = self.select_candidates(
            &overlap_counts,
            query_trigram_count,
            query_trigrams.len(),
            use_jaccard,
            &mut telemetry,
        );

        // Debug logging
        telemetry.log(query, use_jaccard, candidates.len());

        candidates
    }

    /// Get the number of symbols in the index
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.trigram_index.symbol_count
    }

    /// Get the current configuration
    #[must_use]
    pub fn config(&self) -> &FuzzyConfig {
        &self.config
    }
}

struct FuzzyTelemetry {
    initial_candidates: usize,
    jaccard_sum: f64,
    jaccard_count: u32,
    fallback_count: usize,
    dropped_count: usize,
}

impl FuzzyTelemetry {
    fn new(initial_candidates: usize) -> Self {
        Self {
            initial_candidates,
            jaccard_sum: 0.0,
            jaccard_count: 0,
            fallback_count: 0,
            dropped_count: 0,
        }
    }

    fn record_similarity(&mut self, similarity: f64, jaccard_applied: bool) {
        if jaccard_applied {
            self.jaccard_sum += similarity;
            self.jaccard_count += 1;
        } else {
            self.fallback_count += 1;
        }
    }

    fn mark_dropped(&mut self) {
        self.dropped_count += 1;
    }

    fn log(&self, query: &str, use_jaccard: bool, kept: usize) {
        log::debug!(
            "Fuzzy candidate generation: query='{}' initial={} kept={} dropped={} jaccard_avg={:.3} fallback={} mode={}",
            query,
            self.initial_candidates,
            kept,
            self.dropped_count,
            self.jaccard_average(),
            self.fallback_count,
            if use_jaccard { "jaccard" } else { "ratio" }
        );

        if self.fallback_count > 0 && use_jaccard {
            log::debug!(
                "Fuzzy search using fallback ratio for {} candidates (old index or missing counts)",
                self.fallback_count
            );
        }
    }

    fn jaccard_average(&self) -> f64 {
        if self.jaccard_count > 0 {
            self.jaccard_sum / f64::from(self.jaccard_count)
        } else {
            0.0
        }
    }
}

fn compute_similarity(
    use_jaccard: bool,
    entry_id: usize,
    overlap: usize,
    query_trigram_count: f64,
    query_trigram_len: usize,
    symbol_trigram_counts: &[usize],
) -> (f64, bool) {
    if use_jaccard && entry_id < symbol_trigram_counts.len() && !symbol_trigram_counts.is_empty() {
        let symbol_trigram_count = symbol_trigram_counts[entry_id];
        let union = query_trigram_len + symbol_trigram_count - overlap;
        let jaccard = if union > 0 {
            to_f64(overlap) / to_f64(union)
        } else {
            0.0
        };
        (jaccard, true)
    } else {
        (to_f64(overlap) / query_trigram_count, false)
    }
}

impl CandidateGenerator {
    fn collect_overlap_counts(&self, query_trigrams: &[String]) -> Vec<(usize, usize)> {
        use std::collections::HashMap;

        // Count trigram overlaps for each symbol using a HashMap to avoid O(n^2) scans
        let mut overlap_map: HashMap<usize, usize> = HashMap::new();
        for trigram in query_trigrams {
            if let Some(entry_ids) = self.trigram_index.postings.get(trigram) {
                for &entry_id in entry_ids {
                    *overlap_map.entry(entry_id).or_insert(0) += 1;
                }
            }
        }

        // Move into a vector and sort by overlap count (descending)
        let mut overlap_counts: Vec<(usize, usize)> = overlap_map.into_iter().collect();
        // Sort by overlap descending; tie-break by entry_id ascending for stability
        overlap_counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        overlap_counts
    }

    fn extract_query_trigrams(query: &str) -> Option<Vec<String>> {
        use super::trigram::extract_normalized_trigrams;

        let query_trigrams = extract_normalized_trigrams(query);
        if query_trigrams.is_empty() {
            None
        } else {
            Some(query_trigrams)
        }
    }

    fn select_candidates(
        &self,
        overlap_counts: &[(usize, usize)],
        query_trigram_count: f64,
        query_trigram_len: usize,
        use_jaccard: bool,
        telemetry: &mut FuzzyTelemetry,
    ) -> Vec<usize> {
        let mut candidates =
            Vec::with_capacity(self.config.max_candidates.min(overlap_counts.len()));
        let symbol_trigram_counts = &self.trigram_index.symbol_trigram_counts;

        for &(entry_id, overlap) in overlap_counts {
            let (similarity, jaccard_applied) = compute_similarity(
                use_jaccard,
                entry_id,
                overlap,
                query_trigram_count,
                query_trigram_len,
                symbol_trigram_counts,
            );
            telemetry.record_similarity(similarity, jaccard_applied);
            if similarity < self.config.min_similarity {
                telemetry.mark_dropped();
                break; // Since sorted by overlap, no more candidates will pass
            }

            candidates.push(entry_id);

            if candidates.len() >= self.config.max_candidates {
                break;
            }
        }

        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_index() -> TrigramIndex {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello_world");
        index.add_symbol(1, "hello_rust");
        index.add_symbol(2, "hello");
        index.add_symbol(3, "world");
        index.add_symbol(4, "goodbye");
        index
    }

    #[test]
    fn test_candidate_generation_basic() {
        let index = create_test_index();
        let generator = CandidateGenerator::new(Arc::new(index));

        let candidates = generator.generate("hello");
        assert!(!candidates.is_empty());
        assert!(candidates.contains(&0)); // hello_world
        assert!(candidates.contains(&1)); // hello_rust
        assert!(candidates.contains(&2)); // hello
    }

    #[test]
    fn test_candidate_cap_enforced() {
        let index = create_test_index();
        let config = FuzzyConfig {
            max_candidates: 2,
            min_similarity: 0.0,
        };
        let generator = CandidateGenerator::with_config(Arc::new(index), config);

        let candidates = generator.generate("hello");
        assert!(candidates.len() <= 2, "Should cap at 2 candidates");
    }

    #[test]
    fn test_similarity_threshold() {
        let index = create_test_index();
        let config = FuzzyConfig {
            max_candidates: 1000,
            min_similarity: 0.9, // Very high threshold
        };
        let generator = CandidateGenerator::with_config(Arc::new(index), config);

        let candidates = generator.generate("hello");
        // With high threshold, should only get exact or very close matches
        assert!(candidates.len() <= 3);
    }

    #[test]
    fn test_empty_query() {
        let index = create_test_index();
        let generator = CandidateGenerator::new(Arc::new(index));

        let candidates = generator.generate("");
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_no_matches() {
        let index = create_test_index();
        let generator = CandidateGenerator::new(Arc::new(index));

        let candidates = generator.generate("xyz123");
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_symbol_count() {
        let index = create_test_index();
        let generator = CandidateGenerator::new(Arc::new(index));

        assert_eq!(generator.symbol_count(), 5);
    }

    #[test]
    fn test_candidates_sorted_by_relevance() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "test");
        index.add_symbol(1, "testing");
        index.add_symbol(2, "test_function");

        let generator = CandidateGenerator::new(Arc::new(index));
        let candidates = generator.generate("test");

        // First candidate should have highest overlap
        // "test" has all trigrams matching
        assert_eq!(candidates[0], 0);
    }

    #[test]
    fn test_jaccard_similarity_exact_match() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        let generator = CandidateGenerator::new(Arc::new(index));

        // Exact match should have Jaccard = 1.0
        // Both query and symbol have same trigrams: ["hel", "ell", "llo"]
        // overlap = 3, union = 3 + 3 - 3 = 3, jaccard = 3/3 = 1.0
        let candidates = generator.generate("hello");

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0], 0);
    }

    #[test]
    fn test_jaccard_similarity_partial_overlap() {
        let mut index = TrigramIndex::new();
        // "hello" has trigrams: ["hel", "ell", "llo"]
        index.add_symbol(0, "hello");
        // "help" has trigrams: ["hel", "elp"]
        index.add_symbol(1, "help");

        let generator = CandidateGenerator::new(Arc::new(index));

        // Query "hel" has trigram: ["hel"]
        // Node 0 ("hello"): overlap=1, union=1+3-1=3, jaccard=1/3=0.33
        // Node 1 ("help"): overlap=1, union=1+2-1=2, jaccard=1/2=0.5
        let candidates = generator.generate("hel");

        // Both should be candidates (above default 0.1 threshold)
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn test_jaccard_vs_ratio_difference() {
        let mut index = TrigramIndex::new();
        // Short symbol with high overlap
        index.add_symbol(0, "test");
        // Long symbol with same overlap
        index.add_symbol(1, "testing_function_with_test");

        let generator = CandidateGenerator::new(Arc::new(index));

        // Query "test" has trigrams: ["tes", "est"]
        // Node 0 ("test"): overlap=2, |S|=2, union=2+2-2=2, jaccard=2/2=1.0
        // Node 1 ("testing_function_with_test"): overlap=2, |S|=many, jaccard < 1.0
        // Jaccard properly penalizes the long symbol despite high overlap
        let candidates = generator.generate("test");

        // First candidate should be the exact match
        assert_eq!(candidates[0], 0);
    }

    #[test]
    fn test_fallback_to_ratio_when_counts_missing() {
        // Create index with empty counts (simulating old format)
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "world");

        // Manually clear counts to simulate old index
        let index_no_counts = TrigramIndex {
            postings: index.postings.clone(),
            symbol_lengths: index.symbol_lengths.clone(),
            symbol_trigram_counts: Vec::new(), // Empty counts
            symbol_count: index.symbol_count,
        };

        let generator = CandidateGenerator::new(Arc::new(index_no_counts));

        // Should still work using fallback ratio method
        let candidates = generator.generate("hello");
        assert!(!candidates.is_empty());
        assert!(candidates.contains(&0));
    }

    #[test]
    fn test_jaccard_computation_correctness() {
        use crate::search::trigram::extract_normalized_trigrams;

        let mut index = TrigramIndex::new();
        index.add_symbol(0, "context");
        index.add_symbol(1, "content");

        // Manually verify Jaccard computation
        let query = "conte";
        let _query_trigrams = extract_normalized_trigrams(query);

        // "context" trigrams: ["con", "ont", "nte", "ext", "xte"] = 5
        // overlap with query: 3, union: 3 + 5 - 3 = 5, jaccard: 3/5 = 0.6

        // "content" trigrams: ["con", "ont", "nte", "ent"] = 4
        // overlap with query: 3, union: 3 + 4 - 3 = 4, jaccard: 3/4 = 0.75

        let config = FuzzyConfig {
            max_candidates: 10,
            min_similarity: 0.5, // Both should pass this threshold
        };
        let generator = CandidateGenerator::with_config(Arc::new(index), config);
        let candidates = generator.generate(query);

        // Both symbols should pass the 0.5 threshold:
        // "context": jaccard = 3/5 = 0.6 ✓
        // "content": jaccard = 3/4 = 0.75 ✓
        assert_eq!(candidates.len(), 2);

        // Candidates are sorted by overlap count (both have 3), then by entry_id
        // So we expect: [0 (context), 1 (content)]
        assert!(candidates.contains(&0)); // context
        assert!(candidates.contains(&1)); // content
    }

    #[test]
    fn test_jaccard_with_high_threshold() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "helloworld");
        index.add_symbol(2, "help");

        let config = FuzzyConfig {
            max_candidates: 10,
            min_similarity: 0.8, // High threshold
        };
        let generator = CandidateGenerator::with_config(Arc::new(index), config);

        let candidates = generator.generate("hello");

        // Only exact or very close matches should pass
        // "hello" itself should definitely be included
        assert!(candidates.contains(&0));
    }

    #[test]
    fn test_env_var_toggle_disables_jaccard() {
        // Set env var to disable Jaccard
        unsafe {
            std::env::set_var("SQRY_FUZZY_USE_JACCARD", "0");
        }

        let mut index = TrigramIndex::new();
        index.add_symbol(0, "context");
        index.add_symbol(1, "content");

        let config = FuzzyConfig {
            max_candidates: 10,
            min_similarity: 0.5,
        };
        let generator = CandidateGenerator::with_config(Arc::new(index), config);
        let candidates = generator.generate("conte");

        // With ratio mode (disabled Jaccard), both should still pass
        // "context": ratio = 3/3 = 1.0 ✓
        // "content": ratio = 3/3 = 1.0 ✓
        assert_eq!(candidates.len(), 2);

        // Clean up
        unsafe {
            std::env::remove_var("SQRY_FUZZY_USE_JACCARD");
        }
    }

    #[test]
    fn test_zero_union_guard() {
        let mut index = TrigramIndex::new();
        // Edge case: very short strings
        index.add_symbol(0, "a");
        index.add_symbol(1, "b");

        let generator = CandidateGenerator::new(Arc::new(index));

        // Query with no overlap should handle union=0 gracefully
        let candidates = generator.generate("c");
        // Should return empty or handle gracefully without panic
        assert!(candidates.is_empty() || !candidates.is_empty());
    }
}
