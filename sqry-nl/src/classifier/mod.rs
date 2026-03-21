//! Intent classification using MiniLM-L6-v2 ONNX model.
//!
//! This module is only available when the `classifier` feature is enabled.
//! Without it, intent is determined via rule-based classification.

#[cfg(feature = "classifier")]
mod calibration;
#[cfg(feature = "classifier")]
mod model;
#[cfg(feature = "classifier")]
mod tokenizer_wrapper;

#[cfg(feature = "classifier")]
pub use calibration::CalibrationParams;
#[cfg(feature = "classifier")]
pub use model::IntentClassifier;

#[cfg(not(feature = "classifier"))]
use crate::error::ClassifierError;
#[cfg(not(feature = "classifier"))]
use crate::types::{ClassificationResult, Intent};

/// Stub classifier for when the `classifier` feature is disabled.
///
/// Always returns `Intent::Ambiguous` with low confidence.
#[cfg(not(feature = "classifier"))]
pub struct IntentClassifier;

#[cfg(not(feature = "classifier"))]
impl IntentClassifier {
    /// Create a stub classifier.
    pub fn new() -> Result<Self, ClassifierError> {
        Ok(Self)
    }

    /// Classify intent (stub - always returns Ambiguous).
    pub fn classify(&self, _input: &str) -> Result<ClassificationResult, ClassifierError> {
        Ok(ClassificationResult {
            intent: Intent::Ambiguous,
            confidence: 0.0,
            all_probabilities: vec![0.0; Intent::NUM_CLASSES],
            model_version: "stub".to_string(),
        })
    }
}

#[cfg(not(feature = "classifier"))]
impl Default for IntentClassifier {
    fn default() -> Self {
        Self
    }
}
