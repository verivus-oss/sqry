//! Confidence calibration using temperature scaling.
//!
//! Temperature scaling is a simple post-hoc calibration technique that
//! divides logits by a learned temperature parameter before softmax.

use crate::types::Intent;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Calibration parameters for confidence scaling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibrationParams {
    /// Temperature for softmax scaling (default: 1.0 = no scaling)
    pub temperature: f32,
    /// Per-intent confidence thresholds
    pub per_intent_thresholds: HashMap<String, f32>,
}

impl Default for CalibrationParams {
    fn default() -> Self {
        let mut thresholds = HashMap::new();
        thresholds.insert(Intent::SymbolQuery.as_str().to_string(), 0.70);
        thresholds.insert(Intent::TextSearch.as_str().to_string(), 0.70);
        thresholds.insert(Intent::TracePath.as_str().to_string(), 0.75);
        thresholds.insert(Intent::FindCallers.as_str().to_string(), 0.75);
        thresholds.insert(Intent::FindCallees.as_str().to_string(), 0.75);
        thresholds.insert(Intent::Visualize.as_str().to_string(), 0.75);
        thresholds.insert(Intent::IndexStatus.as_str().to_string(), 0.70);
        thresholds.insert(Intent::Ambiguous.as_str().to_string(), 0.50);

        Self {
            temperature: 1.0,
            per_intent_thresholds: thresholds,
        }
    }
}

impl CalibrationParams {
    /// Apply temperature scaling to logits and return probabilities.
    #[must_use]
    pub fn apply_temperature_scaling(&self, logits: &[f32]) -> Vec<f32> {
        // Scale logits by temperature
        let scaled: Vec<f32> = logits.iter().map(|l| l / self.temperature).collect();
        softmax(&scaled)
    }

    /// Check if confidence meets threshold for the given intent.
    #[must_use]
    pub fn meets_threshold(&self, intent: Intent, confidence: f32) -> bool {
        let threshold = self
            .per_intent_thresholds
            .get(intent.as_str())
            .copied()
            .unwrap_or(0.70);
        confidence >= threshold
    }

    /// Get the threshold for an intent.
    #[must_use]
    pub fn threshold_for(&self, intent: Intent) -> f32 {
        self.per_intent_thresholds
            .get(intent.as_str())
            .copied()
            .unwrap_or(0.70)
    }
}

/// Compute softmax of a vector of logits.
#[must_use]
pub fn softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }

    // Subtract max for numerical stability
    let max_logit = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|l| (l - max_logit).exp()).collect();
    let sum_exp: f32 = exps.iter().sum();

    if sum_exp == 0.0 {
        // Avoid division by zero
        let len = u16::try_from(logits.len()).unwrap_or(u16::MAX);
        return vec![1.0 / f32::from(len); logits.len()];
    }

    exps.iter().map(|e| e / sum_exp).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_softmax_basic() {
        let logits = vec![1.0, 2.0, 3.0];
        let probs = softmax(&logits);

        // Sum should be ~1.0
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);

        // Larger logit should have higher probability
        assert!(probs[2] > probs[1]);
        assert!(probs[1] > probs[0]);
    }

    #[test]
    fn test_softmax_empty() {
        let probs = softmax(&[]);
        assert!(probs.is_empty());
    }

    #[test]
    fn test_softmax_numerical_stability() {
        // Large logits that would overflow without max subtraction
        let logits = vec![1000.0, 1001.0, 1002.0];
        let probs = softmax(&logits);

        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_temperature_scaling() {
        let params = CalibrationParams {
            temperature: 2.0,
            ..Default::default()
        };

        let logits = vec![1.0, 2.0, 3.0];
        let probs_scaled = params.apply_temperature_scaling(&logits);
        let probs_unscaled = softmax(&logits);

        // Higher temperature -> more uniform distribution
        let variance_scaled: f32 = probs_scaled.iter().map(|p| (p - 0.333).powi(2)).sum();
        let variance_unscaled: f32 = probs_unscaled.iter().map(|p| (p - 0.333).powi(2)).sum();
        assert!(variance_scaled < variance_unscaled);
    }

    #[test]
    fn test_meets_threshold() {
        let params = CalibrationParams::default();

        assert!(params.meets_threshold(Intent::SymbolQuery, 0.75));
        assert!(!params.meets_threshold(Intent::SymbolQuery, 0.65));
        assert!(params.meets_threshold(Intent::TracePath, 0.80));
        assert!(!params.meets_threshold(Intent::TracePath, 0.70));
    }
}
