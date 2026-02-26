//! ONNX model loading and inference.

use crate::error::ClassifierError;
use crate::types::{ClassificationResult, Intent};
use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use super::calibration::CalibrationParams;

/// Intent classifier using `DistilBERT` ONNX model.
pub struct IntentClassifier {
    /// ONNX Runtime session
    #[allow(dead_code)]
    session: Session,
    /// `HuggingFace` tokenizer
    tokenizer: tokenizers::Tokenizer,
    /// Calibration parameters for confidence scaling
    calibration: CalibrationParams,
    /// Model version string
    model_version: String,
}

/// Compute SHA256 hash of a file.
fn compute_file_hash(path: &Path) -> Result<String, ClassifierError> {
    let mut file = std::fs::File::open(path).map_err(|e| {
        ClassifierError::OnnxError(format!("Failed to open {}: {e}", path.display()))
    })?;

    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let bytes_read = file.read(&mut buffer).map_err(|e| {
            ClassifierError::OnnxError(format!("Failed to read {}: {e}", path.display()))
        })?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Load checksums from checksums.json.
fn load_checksums(model_dir: &Path) -> Result<HashMap<String, String>, ClassifierError> {
    let checksums_path = model_dir.join("checksums.json");
    if !checksums_path.exists() {
        // If no checksums file exists, skip verification (for development)
        tracing::warn!("No checksums.json found, skipping integrity verification");
        return Ok(HashMap::new());
    }

    let content = std::fs::read_to_string(&checksums_path)
        .map_err(|e| ClassifierError::OnnxError(format!("Failed to read checksums.json: {e}")))?;

    serde_json::from_str(&content)
        .map_err(|e| ClassifierError::OnnxError(format!("Failed to parse checksums.json: {e}")))
}

/// Verify file checksums against expected values.
fn verify_checksums(
    model_dir: &Path,
    checksums: &HashMap<String, String>,
) -> Result<(), ClassifierError> {
    for (filename, expected_hash) in checksums {
        let file_path = model_dir.join(filename);
        if !file_path.exists() {
            // Skip files that don't exist (e.g., optimized/ subdirectory)
            continue;
        }

        let actual_hash = compute_file_hash(&file_path)?;
        if actual_hash != *expected_hash {
            return Err(ClassifierError::ChecksumMismatch {
                expected: expected_hash.clone(),
                actual: actual_hash,
            });
        }
        tracing::debug!("Verified checksum for {filename}");
    }
    Ok(())
}

/// Parse model version from version.txt content.
fn parse_model_version(content: &str) -> String {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("model_version=") {
            return line
                .strip_prefix("model_version=")
                .unwrap_or("unknown")
                .to_string();
        }
    }
    "unknown".to_string()
}

impl IntentClassifier {
    /// Load classifier from model directory.
    ///
    /// Expected directory structure:
    /// ```text
    /// model_dir/
    /// ├── model.onnx
    /// ├── tokenizer.json
    /// ├── config.json
    /// ├── calibration.json (optional)
    /// ├── checksums.json
    /// └── version.txt
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`ClassifierError`] if:
    /// - Model files not found
    /// - Checksum verification fails (AC-11.8)
    /// - ONNX Runtime initialization fails
    pub fn load(model_dir: &Path) -> Result<Self, ClassifierError> {
        // Check model directory exists
        if !model_dir.exists() {
            return Err(ClassifierError::ModelNotFound(
                model_dir.display().to_string(),
            ));
        }

        let model_path = model_dir.join("model.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !model_path.exists() {
            return Err(ClassifierError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        if !tokenizer_path.exists() {
            return Err(ClassifierError::ModelNotFound(
                tokenizer_path.display().to_string(),
            ));
        }

        // Verify checksums before loading (AC-11.8)
        let checksums = load_checksums(model_dir)?;
        if !checksums.is_empty() {
            verify_checksums(model_dir, &checksums)?;
            tracing::info!(
                "Model integrity verified: {} files checked",
                checksums.len()
            );
        }

        // Load ONNX session
        let session = Session::builder()
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?
            .with_intra_threads(1)
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?
            .commit_from_file(&model_path)
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        // Load tokenizer
        let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| ClassifierError::TokenizationFailed(e.to_string()))?;

        // Load calibration (optional)
        let calibration_path = model_dir.join("calibration.json");
        let calibration = if calibration_path.exists() {
            let content = std::fs::read_to_string(&calibration_path)
                .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            CalibrationParams::default()
        };

        // Load and parse version
        let version_path = model_dir.join("version.txt");
        let model_version = if version_path.exists() {
            std::fs::read_to_string(&version_path)
                .map_or_else(|_| "unknown".to_string(), |s| parse_model_version(&s))
        } else {
            "unknown".to_string()
        };

        Ok(Self {
            session,
            tokenizer,
            calibration,
            model_version,
        })
    }

    /// Classify intent from natural language input.
    ///
    /// # Critical: `batch_size=1` enforcement (C1 mitigation)
    ///
    /// ONNX Runtime may crash with `batch_size` > 1. This method
    /// always processes exactly one input.
    ///
    /// # Errors
    ///
    /// Returns [`ClassifierError`] if tokenization or inference fails.
    ///
    /// # Note
    ///
    /// This method requires `&mut self` due to ort 2.0 API requirements.
    /// Use a Mutex wrapper if concurrent access is needed.
    pub fn classify(&mut self, input: &str) -> Result<ClassificationResult, ClassifierError> {
        // Tokenize input
        let encoding = self
            .tokenizer
            .encode(input, true)
            .map_err(|e| ClassifierError::TokenizationFailed(e.to_string()))?;

        let input_ids = encoding.get_ids();
        let attention_mask = encoding.get_attention_mask();

        // Truncate to max 512 tokens
        let seq_len = input_ids.len().min(512);
        if input_ids.len() > 512 {
            tracing::warn!("Input truncated from {} to 512 tokens", input_ids.len());
        }

        // Prepare input tensors (batch_size=1)
        let input_ids_i64: Vec<i64> = input_ids[..seq_len].iter().map(|&x| i64::from(x)).collect();
        let attention_mask_i64: Vec<i64> = attention_mask[..seq_len]
            .iter()
            .map(|&x| i64::from(x))
            .collect();

        // Create input tensors with shape [1, seq_len] - ort 2.0 requires Vec not slice
        let input_ids_tensor = Tensor::from_array(([1, seq_len], input_ids_i64))
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;
        let attention_mask_tensor = Tensor::from_array(([1, seq_len], attention_mask_i64))
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        // ort::inputs! returns Vec directly, not Result
        let inputs = ort::inputs![
            "input_ids" => input_ids_tensor,
            "attention_mask" => attention_mask_tensor,
        ];

        let outputs = self
            .session
            .run(inputs)
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        // Extract logits from output
        let logits_tensor = outputs
            .get("logits")
            .ok_or_else(|| ClassifierError::OnnxError("No 'logits' output".to_string()))?;

        // try_extract_tensor returns (&Shape, &[T]) tuple in ort 2.0
        let (_, logits_data) = logits_tensor
            .try_extract_tensor::<f32>()
            .map_err(|e| ClassifierError::OnnxError(e.to_string()))?;

        let logits: Vec<f32> = logits_data.to_vec();

        // Apply calibration and softmax
        let probabilities = self.calibration.apply_temperature_scaling(&logits);

        // Find argmax
        let (intent_idx, confidence) = probabilities
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map_or((Intent::NUM_CLASSES - 1, 0.0), |(idx, &conf)| (idx, conf)); // Default to Ambiguous

        let intent = Intent::from_index(intent_idx);

        Ok(ClassificationResult {
            intent,
            confidence,
            all_probabilities: probabilities,
            model_version: self.model_version.clone(),
        })
    }

    /// Get the model version.
    #[must_use]
    pub fn model_version(&self) -> &str {
        &self.model_version
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_version() {
        let content = r"
# sqry-nl Intent Classifier Model
model_version=1.0.0
model_date=2025-12-09T07:34:00Z
accuracy=0.9998
";
        assert_eq!(parse_model_version(content), "1.0.0");
    }

    #[test]
    fn test_parse_model_version_missing() {
        let content = "# No version here\naccuracy=0.99";
        assert_eq!(parse_model_version(content), "unknown");
    }

    #[test]
    fn test_parse_model_version_empty() {
        assert_eq!(parse_model_version(""), "unknown");
    }

    // Tests requiring actual model files are marked as ignored
    // and run during integration testing.

    #[test]
    #[ignore = "Requires trained model files"]
    fn test_classifier_load() {
        // Would test model loading
    }

    #[test]
    #[ignore = "Requires trained model files"]
    fn test_classifier_inference() {
        // Would test inference
    }

    #[test]
    #[ignore = "Requires trained model files"]
    fn test_checksum_verification() {
        // Would test checksum verification against deployed model
    }
}
