//! Trigram-based indexing for fast fuzzy search candidate generation.
//!
//! This module provides utilities for extracting trigrams from symbol names
//! and building an inverted index that maps trigrams to symbol IDs. This enables
//! fast candidate filtering before running expensive fuzzy matching algorithms.

use crate::search::simd;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Normalize a string for trigram extraction.
///
/// Normalization steps:
/// 1. Convert to lowercase for case-insensitive matching (SIMD-accelerated for ASCII)
/// 2. Trim whitespace
/// 3. Collapse multiple spaces to single space
///
/// # Performance
/// Uses SIMD-accelerated lowercase conversion for ASCII-only strings (3-30x speedup).
/// Falls back to stdlib for Unicode symbols (rare but correct).
///
/// # Examples
///
/// ```
/// use sqry_core::search::trigram::normalize;
///
/// assert_eq!(normalize("HelloWorld"), "helloworld");
/// assert_eq!(normalize("  foo  bar  "), "foo bar");
/// assert_eq!(normalize("CamelCase"), "camelcase");
/// ```
#[must_use]
pub fn normalize(s: &str) -> String {
    // SIMD-accelerated lowercase for ASCII symbols (typical case)
    // Falls back to stdlib for Unicode identifiers (rare, e.g., Python/JS)
    let lowered = if s.is_ascii() {
        simd::to_lowercase_ascii(s)
    } else {
        s.to_lowercase()
    };

    lowered.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract trigrams from a normalized string.
///
/// Trigrams are consecutive 3-character sequences. For strings shorter than 3 chars,
/// the entire string is returned as a single "trigram". Duplicate trigrams are removed.
///
/// # Examples
///
/// ```
/// use sqry_core::search::trigram::extract_trigrams;
///
/// let trigrams = extract_trigrams("hello");
/// assert_eq!(trigrams, vec!["hel", "ell", "llo"]);
///
/// let trigrams = extract_trigrams("ab");
/// assert_eq!(trigrams, vec!["ab"]);
///
/// let trigrams = extract_trigrams("aaa");
/// assert_eq!(trigrams, vec!["aaa"]);
/// ```
#[must_use]
pub fn extract_trigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();

    if chars.len() < 3 {
        return vec![s.to_string()];
    }

    let mut trigrams = Vec::new();
    let mut seen = HashSet::new();

    for i in 0..=chars.len().saturating_sub(3) {
        let trigram: String = chars[i..i + 3].iter().collect();
        if seen.insert(trigram.clone()) {
            trigrams.push(trigram);
        }
    }

    trigrams
}

/// Extract trigrams from a string with normalization.
///
/// Convenience function that combines normalization and trigram extraction.
///
/// # Examples
///
/// ```
/// use sqry_core::search::trigram::extract_normalized_trigrams;
///
/// let trigrams = extract_normalized_trigrams("HelloWorld");
/// assert!(trigrams.contains(&"hel".to_string()));
/// assert!(trigrams.contains(&"wor".to_string()));
/// ```
#[must_use]
pub fn extract_normalized_trigrams(s: &str) -> Vec<String> {
    extract_trigrams(&normalize(s))
}

/// Trigram inverted index for fast candidate retrieval.
///
/// Maps each trigram to a set of symbol IDs that contain it.
/// Also stores the length of each symbol for similarity scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrigramIndex {
    /// Maps trigram -> set of symbol IDs containing it
    pub postings: HashMap<String, Vec<usize>>,

    /// Maps symbol ID -> character length of normalized symbol name
    pub symbol_lengths: Vec<usize>,

    /// Maps symbol ID -> number of unique trigrams extracted from symbol name
    /// Used for Jaccard similarity computation in fuzzy candidate filtering.
    /// Backward compatible: defaults to empty vec for old indices.
    #[serde(default)]
    pub symbol_trigram_counts: Vec<usize>,

    /// Total number of active symbols indexed
    pub symbol_count: usize,
}

impl TrigramIndex {
    /// Create a new empty trigram index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            postings: HashMap::new(),
            symbol_lengths: Vec::new(),
            symbol_trigram_counts: Vec::new(),
            symbol_count: 0,
        }
    }

    /// Add an entry to the index.
    ///
    /// # Arguments
    ///
    /// * `entry_id` - Unique identifier for this entry
    /// * `name` - Name to extract trigrams from
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::search::trigram::TrigramIndex;
    ///
    /// let mut index = TrigramIndex::new();
    /// index.add_symbol(0, "hello");
    /// index.add_symbol(1, "world");
    ///
    /// assert_eq!(index.symbol_count, 2);
    /// ```
    pub fn add_symbol(&mut self, entry_id: usize, name: &str) {
        let normalized = normalize(name);
        let trigrams = extract_trigrams(&normalized);
        let trigram_count = trigrams.len();

        // Ensure symbol_lengths and symbol_trigram_counts vectors are large enough
        if entry_id >= self.symbol_lengths.len() {
            self.symbol_lengths.resize(entry_id + 1, 0);
            self.symbol_trigram_counts.resize(entry_id + 1, 0);
        }
        self.symbol_lengths[entry_id] = normalized.chars().count();
        self.symbol_trigram_counts[entry_id] = trigram_count;

        // Add entry_id to posting list for each trigram
        for trigram in trigrams {
            self.postings.entry(trigram).or_default().push(entry_id);
        }

        self.symbol_count = self.symbol_count.saturating_add(1);
    }

    /// Remove an entry from the index. No-op if the entry was not present.
    pub fn remove_symbol(&mut self, entry_id: usize, name: &str) {
        let normalized = normalize(name);
        let trigrams = extract_trigrams(&normalized);

        for trigram in trigrams {
            if let Some(postings) = self.postings.get_mut(&trigram) {
                if let Some(pos) = postings.iter().position(|&id| id == entry_id) {
                    postings.swap_remove(pos);
                }
                if postings.is_empty() {
                    self.postings.remove(&trigram);
                }
            }
        }

        let mut removed = false;
        if entry_id < self.symbol_lengths.len() {
            removed = self.symbol_lengths[entry_id] != 0;
            self.symbol_lengths[entry_id] = 0;
        }
        if entry_id < self.symbol_trigram_counts.len() {
            self.symbol_trigram_counts[entry_id] = 0;
        }

        if removed {
            self.symbol_count = self.symbol_count.saturating_sub(1);
        }
    }

    /// Get candidate entry IDs that share trigrams with the query.
    ///
    /// Returns entry IDs sorted by number of matching trigrams (descending).
    ///
    /// # Arguments
    ///
    /// * `query` - Query string to match against
    /// * `min_overlap` - Minimum number of trigrams that must match (default: 1)
    ///
    /// # Examples
    ///
    /// ```
    /// use sqry_core::search::trigram::TrigramIndex;
    ///
    /// let mut index = TrigramIndex::new();
    /// index.add_symbol(0, "hello");
    /// index.add_symbol(1, "help");
    /// index.add_symbol(2, "world");
    ///
    /// let candidates = index.get_candidates("hel", 1);
    /// assert!(candidates.contains(&0));
    /// assert!(candidates.contains(&1));
    /// assert!(!candidates.contains(&2));
    /// ```
    #[must_use]
    pub fn get_candidates(&self, query: &str, min_overlap: usize) -> Vec<usize> {
        let query_trigrams = extract_normalized_trigrams(query);

        if query_trigrams.is_empty() {
            return Vec::new();
        }

        // Count trigram overlaps for each symbol
        let mut overlap_counts: HashMap<usize, usize> = HashMap::new();

        for trigram in &query_trigrams {
            if let Some(entry_ids) = self.postings.get(trigram) {
                for &entry_id in entry_ids {
                    *overlap_counts.entry(entry_id).or_insert(0) += 1;
                }
            }
        }

        // Filter by minimum overlap and sort by overlap count (descending)
        let mut candidates: Vec<(usize, usize)> = overlap_counts
            .into_iter()
            .filter(|(_, count)| *count >= min_overlap)
            .collect();

        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        candidates.into_iter().map(|(id, _)| id).collect()
    }

    /// Serialize the index to bytes using postcard.
    ///
    /// # Errors
    ///
    /// Returns [`postcard::Error`] when serialization fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_allocvec(self)
    }

    /// Deserialize the index from bytes using postcard.
    ///
    /// # Errors
    ///
    /// Returns [`postcard::Error`] when the buffer cannot be deserialized into a
    /// `TrigramIndex` (corrupted or incompatible data).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, postcard::Error> {
        let mut index: Self = postcard::from_bytes(bytes)?;
        index.symbol_count = index.symbol_lengths.iter().filter(|len| **len > 0).count();
        Ok(index)
    }
}

impl Default for TrigramIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize() {
        assert_eq!(normalize("HelloWorld"), "helloworld");
        assert_eq!(normalize("  spaces  "), "spaces");
        assert_eq!(normalize("CamelCase"), "camelcase");
        assert_eq!(normalize("snake_case"), "snake_case");
        assert_eq!(normalize("UPPERCASE"), "uppercase");
    }

    #[test]
    fn test_extract_trigrams() {
        assert_eq!(extract_trigrams("hello"), vec!["hel", "ell", "llo"]);
        assert_eq!(extract_trigrams("ab"), vec!["ab"]);
        assert_eq!(extract_trigrams("abc"), vec!["abc"]);
        assert_eq!(extract_trigrams(""), vec![""]);
    }

    #[test]
    fn test_extract_trigrams_dedup() {
        // "aaa" produces only one unique trigram
        assert_eq!(extract_trigrams("aaa"), vec!["aaa"]);

        // "abab" produces "aba" and "bab"
        let trigrams = extract_trigrams("abab");
        assert_eq!(trigrams.len(), 2);
        assert!(trigrams.contains(&"aba".to_string()));
        assert!(trigrams.contains(&"bab".to_string()));
    }

    #[test]
    fn test_extract_trigrams_unicode() {
        let trigrams = extract_trigrams("café");
        assert_eq!(trigrams, vec!["caf", "afé"]);
    }

    #[test]
    fn test_extract_normalized_trigrams() {
        let trigrams = extract_normalized_trigrams("HelloWorld");
        assert!(trigrams.contains(&"hel".to_string()));
        assert!(trigrams.contains(&"wor".to_string()));
        assert!(!trigrams.contains(&"Hel".to_string())); // normalized to lowercase
    }

    #[test]
    fn test_trigram_index_add_symbol() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "world");

        assert_eq!(index.symbol_count, 2);
        assert_eq!(index.symbol_lengths.len(), 2);
        assert_eq!(index.symbol_lengths[0], 5); // "hello"
        assert_eq!(index.symbol_lengths[1], 5); // "world"
    }

    #[test]
    fn test_trigram_index_get_candidates() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "help");
        index.add_symbol(2, "world");

        // Query "hel" should match "hello" and "help"
        let candidates = index.get_candidates("hel", 1);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.contains(&0));
        assert!(candidates.contains(&1));

        // Query "world" should match only "world"
        let candidates = index.get_candidates("world", 1);
        assert_eq!(candidates.len(), 1);
        assert!(candidates.contains(&2));

        // Query with no matches
        let candidates = index.get_candidates("xyz", 1);
        assert_eq!(candidates.len(), 0);
    }

    #[test]
    fn test_trigram_index_min_overlap() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "help");
        index.add_symbol(2, "he");

        // Require at least 2 matching trigrams
        let candidates = index.get_candidates("hello", 2);
        assert_eq!(candidates.len(), 1);
        assert!(candidates.contains(&0)); // "hello" matches itself perfectly
    }

    #[test]
    fn test_trigram_index_serialization() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "world");

        // Serialize
        let bytes = index.to_bytes().expect("serialization failed");
        assert!(!bytes.is_empty());

        // Deserialize
        let restored = TrigramIndex::from_bytes(&bytes).expect("deserialization failed");

        // Verify structure is preserved
        assert_eq!(restored.symbol_count, index.symbol_count);
        assert_eq!(restored.symbol_lengths, index.symbol_lengths);
        assert_eq!(restored.postings.len(), index.postings.len());

        // Verify functionality is preserved
        let candidates = restored.get_candidates("hello", 1);
        assert!(candidates.contains(&0));
    }

    #[test]
    fn test_trigram_index_roundtrip() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "Context");
        index.add_symbol(1, "Engine");
        index.add_symbol(2, "HandlerFunc");

        let bytes = index.to_bytes().unwrap();
        let restored = TrigramIndex::from_bytes(&bytes).unwrap();

        // Test exact equality of all fields
        assert_eq!(index.symbol_count, restored.symbol_count);
        assert_eq!(index.symbol_lengths, restored.symbol_lengths);

        // Compare postings (order-independent)
        assert_eq!(index.postings.len(), restored.postings.len());
        for (key, value) in &index.postings {
            let restored_value = restored.postings.get(key).expect("missing key");
            assert_eq!(value, restored_value, "mismatch for key: {key}");
        }
    }

    #[test]
    fn test_symbol_trigram_counts() {
        let mut index = TrigramIndex::new();

        // "hello" -> normalized "hello" -> trigrams: ["hel", "ell", "llo"] = 3
        index.add_symbol(0, "hello");
        assert_eq!(index.symbol_trigram_counts[0], 3);

        // "ab" -> normalized "ab" -> trigrams: ["ab"] = 1 (short string)
        index.add_symbol(1, "ab");
        assert_eq!(index.symbol_trigram_counts[1], 1);

        // "aaa" -> normalized "aaa" -> trigrams: ["aaa"] = 1 (deduplicated)
        index.add_symbol(2, "aaa");
        assert_eq!(index.symbol_trigram_counts[2], 1);

        // "HelloWorld" -> normalized "helloworld" -> multiple unique trigrams
        index.add_symbol(3, "HelloWorld");
        let expected_count = extract_normalized_trigrams("HelloWorld").len();
        assert_eq!(index.symbol_trigram_counts[3], expected_count);
    }

    #[test]
    fn test_symbol_trigram_counts_match_extraction() {
        let mut index = TrigramIndex::new();
        let test_symbols = [
            "hello",
            "world",
            "Context",
            "Engine",
            "fuzzy_search",
            "CamelCase",
        ];

        for (id, symbol) in test_symbols.iter().enumerate() {
            index.add_symbol(id, symbol);

            // Verify count matches actual trigram extraction
            let expected = extract_normalized_trigrams(symbol).len();
            assert_eq!(
                index.symbol_trigram_counts[id], expected,
                "Trigram count mismatch for symbol '{symbol}'"
            );
        }
    }

    #[test]
    fn test_trigram_counts_serialization() {
        let mut index = TrigramIndex::new();
        index.add_symbol(0, "hello");
        index.add_symbol(1, "world");
        index.add_symbol(2, "fuzzy");

        // Serialize
        let bytes = index.to_bytes().expect("serialization failed");

        // Deserialize
        let restored = TrigramIndex::from_bytes(&bytes).expect("deserialization failed");

        // Verify counts are preserved
        assert_eq!(restored.symbol_trigram_counts, index.symbol_trigram_counts);
        assert_eq!(restored.symbol_trigram_counts.len(), 3);
        assert_eq!(restored.symbol_trigram_counts[0], 3); // "hello" -> 3 trigrams
        assert_eq!(restored.symbol_trigram_counts[1], 3); // "world" -> 3 trigrams
    }

    #[test]
    fn test_backward_compatibility_empty_counts() {
        // Simulate old index format by manually creating one without counts
        let old_index = TrigramIndex {
            postings: HashMap::from([
                ("hel".to_string(), vec![0]),
                ("ell".to_string(), vec![0]),
                ("llo".to_string(), vec![0]),
            ]),
            symbol_lengths: vec![5],
            symbol_trigram_counts: Vec::new(), // Empty (old format)
            symbol_count: 1,
        };

        // Serialize and deserialize
        let bytes = old_index.to_bytes().unwrap();
        let restored = TrigramIndex::from_bytes(&bytes).unwrap();

        // Verify it loads successfully with empty counts
        assert_eq!(restored.symbol_trigram_counts.len(), 0);
        assert_eq!(restored.symbol_count, 1);
        assert_eq!(restored.symbol_lengths, vec![5]);
    }

    #[test]
    fn test_serde_default_for_missing_counts() {
        // Create JSON representation without symbol_trigram_counts field
        let json = r#"{
            "postings": {"hel": [0], "ell": [0], "llo": [0]},
            "symbol_lengths": [5],
            "symbol_count": 1
        }"#;

        // Deserialize (should use default for missing field)
        let index: TrigramIndex = serde_json::from_str(json).expect("JSON deserialization failed");

        // Verify default (empty vec) was used
        assert_eq!(index.symbol_trigram_counts.len(), 0);
        assert_eq!(index.symbol_count, 1);
    }
}
