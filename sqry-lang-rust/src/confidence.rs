//! Confidence tracking for Rust analysis results.
//!
//! This module provides infrastructure for tracking the confidence level
//! and limitations of analysis results. This is critical for PROVE framework
//! compliance (Explicit criterion).
//!
//! # Confidence Levels
//!
//! - **Verified**: Full rust-analyzer integration available, results validated
//! - **Partial**: Some features degraded, mixed confidence
//! - **`AstOnly`**: Tree-sitter AST analysis only, no type inference
//!
//! # Example
//!
//! ```ignore
//! let mut tracker = ConfidenceTracker::new(ConfidenceLevel::Partial);
//! tracker.add_limitation("Macro expansion disabled for security");
//! tracker.add_limitation("Elided lifetime inference requires rust-analyzer");
//!
//! let metadata = tracker.to_metadata();
//! // {"level": "partial", "limitations": ["Macro expansion disabled..."]}
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Confidence level for analysis results.
///
/// Indicates how much trust can be placed in the analysis results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    /// Full rust-analyzer integration, results verified against RA
    Verified,
    /// Some features degraded, mixed confidence levels
    #[default]
    Partial,
    /// Tree-sitter AST analysis only, no type inference
    AstOnly,
}

impl ConfidenceLevel {
    /// Returns the string representation for serialization.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Partial => "partial",
            Self::AstOnly => "ast_only",
        }
    }

    /// Parse from string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "verified" => Some(Self::Verified),
            "partial" => Some(Self::Partial),
            "ast_only" => Some(Self::AstOnly),
            _ => None,
        }
    }

    /// Returns true if this level represents full confidence.
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Verified)
    }

    /// Returns true if this level is AST-only (lowest confidence).
    #[must_use]
    pub const fn is_ast_only(self) -> bool {
        matches!(self, Self::AstOnly)
    }
}

impl std::fmt::Display for ConfidenceLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Confidence metadata for analysis results.
///
/// This is the canonical JSON schema for confidence indicators as
/// defined in the PROVE framework compliance requirements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfidenceMetadata {
    /// The overall confidence level
    pub level: ConfidenceLevel,

    /// List of limitations affecting the analysis
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,

    /// Features that were unavailable during analysis
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_features: Vec<String>,
}

impl Default for ConfidenceMetadata {
    fn default() -> Self {
        Self {
            level: ConfidenceLevel::Partial,
            limitations: Vec::new(),
            unavailable_features: Vec::new(),
        }
    }
}

impl ConfidenceMetadata {
    /// Create new metadata with the given level.
    #[must_use]
    pub fn new(level: ConfidenceLevel) -> Self {
        Self {
            level,
            limitations: Vec::new(),
            unavailable_features: Vec::new(),
        }
    }

    /// Create metadata for AST-only analysis.
    #[must_use]
    pub fn ast_only() -> Self {
        Self {
            level: ConfidenceLevel::AstOnly,
            limitations: vec![
                "Tree-sitter AST analysis only".to_string(),
                "No type inference available".to_string(),
            ],
            unavailable_features: vec![
                "type_inference".to_string(),
                "trait_method_binding".to_string(),
                "elided_lifetimes".to_string(),
            ],
        }
    }

    /// Check if this is verified confidence.
    #[must_use]
    pub fn is_verified(&self) -> bool {
        self.level.is_verified() && self.limitations.is_empty()
    }
}

/// Tracks confidence level and limitations during analysis.
///
/// This is the primary interface for building confidence metadata
/// during graph construction.
#[derive(Debug)]
pub struct ConfidenceTracker {
    level: ConfidenceLevel,
    limitations: HashSet<String>,
    unavailable_features: HashSet<String>,
}

impl Default for ConfidenceTracker {
    fn default() -> Self {
        Self::new(ConfidenceLevel::Partial)
    }
}

impl ConfidenceTracker {
    /// Create a new tracker with the given initial level.
    #[must_use]
    pub fn new(level: ConfidenceLevel) -> Self {
        Self {
            level,
            limitations: HashSet::new(),
            unavailable_features: HashSet::new(),
        }
    }

    /// Create a tracker for AST-only analysis.
    #[must_use]
    pub fn ast_only() -> Self {
        let mut tracker = Self::new(ConfidenceLevel::AstOnly);
        tracker.add_limitation("Tree-sitter AST analysis only");
        tracker.add_limitation("No type inference available");
        tracker.add_unavailable_feature("type_inference");
        tracker.add_unavailable_feature("trait_method_binding");
        tracker.add_unavailable_feature("elided_lifetimes");
        tracker
    }

    /// Create a tracker for verified analysis.
    #[must_use]
    pub fn verified() -> Self {
        Self::new(ConfidenceLevel::Verified)
    }

    /// Get the current confidence level.
    #[must_use]
    pub fn level(&self) -> ConfidenceLevel {
        self.level
    }

    /// Set the confidence level.
    ///
    /// Note: Cannot upgrade from AST-only to a higher level.
    pub fn set_level(&mut self, level: ConfidenceLevel) {
        // Only downgrade, never upgrade
        match (self.level, level) {
            (ConfidenceLevel::Verified, _)
            | (ConfidenceLevel::Partial, ConfidenceLevel::AstOnly) => self.level = level,
            _ => {} // Cannot upgrade
        }
    }

    /// Downgrade to partial confidence.
    pub fn downgrade_to_partial(&mut self) {
        if self.level == ConfidenceLevel::Verified {
            self.level = ConfidenceLevel::Partial;
        }
    }

    /// Downgrade to AST-only confidence.
    pub fn downgrade_to_ast_only(&mut self) {
        self.level = ConfidenceLevel::AstOnly;
    }

    /// Add a limitation message.
    ///
    /// Automatically downgrades from Verified to Partial.
    pub fn add_limitation(&mut self, limitation: &str) {
        if self.level == ConfidenceLevel::Verified {
            self.level = ConfidenceLevel::Partial;
        }
        self.limitations.insert(limitation.to_string());
    }

    /// Add an unavailable feature.
    pub fn add_unavailable_feature(&mut self, feature: &str) {
        self.unavailable_features.insert(feature.to_string());
    }

    /// Check if a specific limitation exists.
    #[must_use]
    pub fn has_limitation(&self, limitation: &str) -> bool {
        self.limitations.contains(limitation)
    }

    /// Get the number of limitations.
    #[must_use]
    pub fn limitation_count(&self) -> usize {
        self.limitations.len()
    }

    /// Convert to metadata for serialization.
    #[must_use]
    pub fn to_metadata(&self) -> ConfidenceMetadata {
        let mut limitations: Vec<_> = self.limitations.iter().cloned().collect();
        limitations.sort();

        let mut unavailable_features: Vec<_> = self.unavailable_features.iter().cloned().collect();
        unavailable_features.sort();

        ConfidenceMetadata {
            level: self.level,
            limitations,
            unavailable_features,
        }
    }

    /// Merge another tracker's limitations into this one.
    ///
    /// Takes the lower confidence level of the two.
    pub fn merge(&mut self, other: &Self) {
        // Take the lower confidence level
        match (self.level, other.level) {
            (_, ConfidenceLevel::AstOnly) => self.level = ConfidenceLevel::AstOnly,
            (ConfidenceLevel::Verified, ConfidenceLevel::Partial) => {
                self.level = ConfidenceLevel::Partial;
            }
            _ => {}
        }

        // Merge limitations
        for limitation in &other.limitations {
            self.limitations.insert(limitation.clone());
        }

        // Merge unavailable features
        for feature in &other.unavailable_features {
            self.unavailable_features.insert(feature.clone());
        }
    }

    /// Clear all limitations and unavailable features.
    pub fn clear(&mut self) {
        self.limitations.clear();
        self.unavailable_features.clear();
    }

    /// Reset to a specific level, clearing all limitations.
    pub fn reset(&mut self, level: ConfidenceLevel) {
        self.level = level;
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_level_as_str() {
        assert_eq!(ConfidenceLevel::Verified.as_str(), "verified");
        assert_eq!(ConfidenceLevel::Partial.as_str(), "partial");
        assert_eq!(ConfidenceLevel::AstOnly.as_str(), "ast_only");
    }

    #[test]
    fn test_confidence_level_parse() {
        assert_eq!(
            ConfidenceLevel::parse("verified"),
            Some(ConfidenceLevel::Verified)
        );
        assert_eq!(
            ConfidenceLevel::parse("partial"),
            Some(ConfidenceLevel::Partial)
        );
        assert_eq!(
            ConfidenceLevel::parse("ast_only"),
            Some(ConfidenceLevel::AstOnly)
        );
        assert_eq!(ConfidenceLevel::parse("unknown"), None);
    }

    #[test]
    fn test_confidence_tracker_basic() {
        let mut tracker = ConfidenceTracker::new(ConfidenceLevel::Verified);
        assert_eq!(tracker.level(), ConfidenceLevel::Verified);
        assert_eq!(tracker.limitation_count(), 0);

        tracker.add_limitation("Test limitation");
        assert_eq!(tracker.level(), ConfidenceLevel::Partial);
        assert_eq!(tracker.limitation_count(), 1);
        assert!(tracker.has_limitation("Test limitation"));
    }

    #[test]
    fn test_confidence_tracker_downgrade() {
        let mut tracker = ConfidenceTracker::verified();
        assert_eq!(tracker.level(), ConfidenceLevel::Verified);

        tracker.downgrade_to_partial();
        assert_eq!(tracker.level(), ConfidenceLevel::Partial);

        tracker.downgrade_to_ast_only();
        assert_eq!(tracker.level(), ConfidenceLevel::AstOnly);

        // Cannot upgrade
        tracker.set_level(ConfidenceLevel::Verified);
        assert_eq!(tracker.level(), ConfidenceLevel::AstOnly);
    }

    #[test]
    fn test_confidence_tracker_merge() {
        let mut tracker1 = ConfidenceTracker::new(ConfidenceLevel::Verified);
        tracker1.add_limitation("Limitation A");

        let mut tracker2 = ConfidenceTracker::new(ConfidenceLevel::AstOnly);
        tracker2.add_limitation("Limitation B");
        tracker2.add_unavailable_feature("feature_x");

        tracker1.merge(&tracker2);

        assert_eq!(tracker1.level(), ConfidenceLevel::AstOnly);
        assert!(tracker1.has_limitation("Limitation A"));
        assert!(tracker1.has_limitation("Limitation B"));
    }

    #[test]
    fn test_confidence_metadata_serialization() {
        let mut tracker = ConfidenceTracker::new(ConfidenceLevel::Partial);
        tracker.add_limitation("Macro expansion disabled");
        tracker.add_unavailable_feature("macro_expansion");

        let metadata = tracker.to_metadata();
        let json = serde_json::to_string(&metadata).unwrap();

        assert!(json.contains("\"partial\""));
        assert!(json.contains("Macro expansion disabled"));
        assert!(json.contains("macro_expansion"));

        // Roundtrip
        let deserialized: ConfidenceMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, metadata);
    }

    #[test]
    fn test_confidence_metadata_ast_only() {
        let metadata = ConfidenceMetadata::ast_only();
        assert_eq!(metadata.level, ConfidenceLevel::AstOnly);
        assert!(!metadata.limitations.is_empty());
        assert!(!metadata.unavailable_features.is_empty());
    }

    #[test]
    fn test_confidence_tracker_ast_only() {
        let tracker = ConfidenceTracker::ast_only();
        assert_eq!(tracker.level(), ConfidenceLevel::AstOnly);
        assert!(tracker.has_limitation("Tree-sitter AST analysis only"));
    }

    #[test]
    fn test_confidence_level_default() {
        assert_eq!(ConfidenceLevel::default(), ConfidenceLevel::Partial);
    }

    #[test]
    fn test_confidence_metadata_is_verified() {
        let verified = ConfidenceMetadata::new(ConfidenceLevel::Verified);
        assert!(verified.is_verified());

        let mut partial = ConfidenceMetadata::new(ConfidenceLevel::Verified);
        partial.limitations.push("Something".to_string());
        assert!(!partial.is_verified());
    }
}
