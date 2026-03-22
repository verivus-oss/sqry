//! Confidence metadata for analysis results.
//!
//! This module provides infrastructure for tracking the confidence level
//! and limitations of analysis results. This is used primarily for Rust
//! analysis where confidence can vary based on available tooling.
//!
//! # Confidence Levels
//!
//! - **Verified**: Full rust-analyzer integration available, results validated
//! - **Partial**: Some features degraded, mixed confidence
//! - **AstOnly**: Tree-sitter AST analysis only, no type inference

use serde::{Deserialize, Serialize};

/// Confidence level for analysis results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceLevel {
    /// Full rust-analyzer integration, results verified
    Verified,
    /// Some features degraded, mixed confidence
    #[default]
    Partial,
    /// Tree-sitter AST analysis only, no type inference
    AstOnly,
}

/// Confidence metadata stored in the manifest and included in query responses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfidenceMetadata {
    /// Overall confidence level
    pub level: ConfidenceLevel,
    /// List of limitations affecting the analysis
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
    /// Features unavailable during analysis
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unavailable_features: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_level_default() {
        let level = ConfidenceLevel::default();
        assert_eq!(level, ConfidenceLevel::Partial);
    }

    #[test]
    fn test_confidence_level_serialization() {
        let verified = ConfidenceLevel::Verified;
        let json = serde_json::to_string(&verified).unwrap();
        assert_eq!(json, "\"verified\"");

        let partial = ConfidenceLevel::Partial;
        let json = serde_json::to_string(&partial).unwrap();
        assert_eq!(json, "\"partial\"");

        let ast_only = ConfidenceLevel::AstOnly;
        let json = serde_json::to_string(&ast_only).unwrap();
        assert_eq!(json, "\"ast_only\"");
    }

    #[test]
    fn test_confidence_level_deserialization() {
        let verified: ConfidenceLevel = serde_json::from_str("\"verified\"").unwrap();
        assert_eq!(verified, ConfidenceLevel::Verified);

        let partial: ConfidenceLevel = serde_json::from_str("\"partial\"").unwrap();
        assert_eq!(partial, ConfidenceLevel::Partial);

        let ast_only: ConfidenceLevel = serde_json::from_str("\"ast_only\"").unwrap();
        assert_eq!(ast_only, ConfidenceLevel::AstOnly);
    }

    #[test]
    fn test_confidence_metadata_default() {
        let meta = ConfidenceMetadata::default();
        assert_eq!(meta.level, ConfidenceLevel::Partial);
        assert!(meta.limitations.is_empty());
        assert!(meta.unavailable_features.is_empty());
    }

    #[test]
    fn test_confidence_metadata_serialization() {
        let meta = ConfidenceMetadata {
            level: ConfidenceLevel::AstOnly,
            limitations: vec!["No type inference".to_string()],
            unavailable_features: vec!["rust-analyzer".to_string()],
        };

        let json = serde_json::to_string(&meta).unwrap();
        assert!(json.contains("\"level\":\"ast_only\""));
        assert!(json.contains("\"limitations\""));
        assert!(json.contains("\"No type inference\""));
        assert!(json.contains("\"unavailable_features\""));
        assert!(json.contains("\"rust-analyzer\""));
    }

    #[test]
    fn test_confidence_metadata_empty_vectors_omitted() {
        let meta = ConfidenceMetadata {
            level: ConfidenceLevel::Verified,
            limitations: vec![],
            unavailable_features: vec![],
        };

        let json = serde_json::to_string(&meta).unwrap();
        // Empty vectors should be omitted due to skip_serializing_if
        assert!(!json.contains("\"limitations\""));
        assert!(!json.contains("\"unavailable_features\""));
        assert!(json.contains("\"level\":\"verified\""));
    }

    #[test]
    fn test_confidence_metadata_roundtrip() {
        let original = ConfidenceMetadata {
            level: ConfidenceLevel::Partial,
            limitations: vec!["Limited accuracy".to_string()],
            unavailable_features: vec!["Type checking".to_string()],
        };

        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ConfidenceMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(original, deserialized);
    }
}
