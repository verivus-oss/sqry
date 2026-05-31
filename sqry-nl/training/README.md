# sqry-nl Training Pipeline

Training scripts for the sqry-nl intent classifier. This pipeline generates training data,
fine-tunes an all-MiniLM-L6-v2 model (22M params), exports to ONNX with INT8 quantization,
and calibrates confidence using temperature scaling.

## Overview

The training pipeline consists of four main stages:

1. **Data Generation** - Template-based synthetic data generation (H6 mitigation)
2. **Model Training** - MiniLM-L6-v2 fine-tuning for intent classification
3. **ONNX Export** - Model conversion with int8 quantization (C2 constraint)
4. **Calibration** - Temperature scaling for confidence calibration (C3 constraint)

## Requirements

Python 3.10+ with the following dependencies:

```bash
pip install -r requirements.txt
```

### Key Dependencies

- `torch>=2.12.0` - PyTorch for model training and ONNX export
- `transformers>=5.0.0` - Hugging Face Transformers
- `onnx>=1.21.0` - ONNX model validation and shape inference
- `onnxruntime>=1.23.2` - ONNX inference runtime and dynamic quantization
- `netcal>=1.3.0` - Calibration metrics

## Quick Start

```bash
# 1. Generate training data
python generate_data.py generate \
  --output data/train.json \
  --samples-per-intent 1000 \
  --verification-output data/verify.json

# 2. Manually verify 10% of samples (H6 requirement)
python generate_data.py verify data/verify.json

# 3. Generate evaluation data (separate seed for independence)
python generate_data.py generate \
  --output data/eval.json \
  --samples-per-intent 1000 \
  --seed 42

# 4. Train the classifier (default: all-MiniLM-L6-v2)
python train_classifier.py train \
  --train data/train.json \
  --eval data/eval.json \
  --output models/intent_classifier \
  --model sentence-transformers/all-MiniLM-L6-v2

# 5. Export to ONNX with quantization
python export_onnx.py export \
  --model models/intent_classifier/final \
  --output models/onnx \
  --quantize \
  --eval data/eval.json

# 6. Calibrate confidence
python calibrate.py calibrate \
  --model models/onnx/quantized/model_quantized.onnx \
  --tokenizer models/onnx \
  --data data/eval.json \
  --output models/temperature.json

# 7. Copy artifacts to sqry-nl
cp models/onnx/quantized/model_quantized.onnx ../models/intent_classifier.onnx
cp models/onnx/{config.json,tokenizer.json,checksums.json} ../models/
cp models/temperature.json ../models/
```

## Scripts

### generate_data.py

Generates training data using template-based augmentation. This approach (H6 mitigation)
avoids LLM-generated data for reproducibility and auditability.

**Commands:**

```bash
# Generate training data
python generate_data.py generate \
  --output data/train.json \
  --samples-per-intent 1000 \
  --augmentation-ratio 0.5 \
  --verification-output data/verify.json

# Interactive verification (H6 requirement: verify 10% of samples)
python generate_data.py verify data/verify.json
```

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--output` | `data/train.json` | Output file path |
| `--samples-per-intent` | 1000 | Samples per intent class |
| `--augmentation-ratio` | 0.5 | Ratio of augmented samples |
| `--seed` | None | Random seed for reproducibility |
| `--verification-output` | None | Output file for verification sample |

### train_classifier.py

Fine-tunes a base model for intent classification. Default is `sentence-transformers/all-MiniLM-L6-v2`
(22M params, BERT architecture with `token_type_ids`).

**Commands:**

```bash
# Train model (default: all-MiniLM-L6-v2)
python train_classifier.py train \
  --train data/train.json \
  --eval data/eval.json \
  --output models/intent_classifier \
  --model sentence-transformers/all-MiniLM-L6-v2 \
  --epochs 3 \
  --batch-size 32

# Evaluate existing model
python train_classifier.py evaluate \
  models/intent_classifier/final \
  data/eval.json

# Predict single text
python train_classifier.py predict \
  models/intent_classifier/final \
  "find authenticate_user"
```

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--train` | Required | Training data JSON file |
| `--eval` | None | Evaluation data JSON file |
| `--output` | `models/intent_classifier` | Output directory |
| `--model` | `sentence-transformers/all-MiniLM-L6-v2` | Base model name |
| `--batch-size` | 32 | Training batch size |
| `--lr` | 2e-5 | Learning rate |
| `--epochs` | 3 | Number of epochs |
| `--fp16` | Auto | Mixed precision training |

### export_onnx.py

Exports PyTorch model to ONNX format with optional int8 quantization.

**Commands:**

```bash
# Basic export
python export_onnx.py export \
  --model models/intent_classifier/final \
  --output models/onnx

# Export with quantization
python export_onnx.py export \
  --model models/intent_classifier/final \
  --output models/onnx \
  --quantize \
  --eval data/eval.json

# Verify ONNX model
python export_onnx.py verify models/onnx/model.onnx

# Benchmark inference latency
python export_onnx.py benchmark \
  models/onnx/quantized/model.onnx \
  --tokenizer models/onnx
```

**Options:**

| Option | Default | Description |
|--------|---------|-------------|
| `--model` | Required | PyTorch model directory |
| `--output` | `models/onnx` | Output directory |
| `--quantize` | False | Apply int8 quantization |
| `--optimize` | True | Apply graph optimizations |
| `--eval` | None | Evaluation data for accuracy comparison |

**Outputs:**

```
models/onnx/
├── model.onnx                 # Base ONNX model
├── optimized/
│   └── model_optimized.onnx   # Optimized model
├── quantized/
│   └── model_quantized.onnx   # INT8 quantized model
├── config.json                # Model configuration
├── tokenizer.json             # Tokenizer
├── tokenizer_config.json
├── vocab.txt
└── checksums.json             # SHA256 checksums (AC-11.8)
```

### calibrate.py

Applies temperature scaling for confidence calibration.

**Commands:**

```bash
# Calibrate model
python calibrate.py calibrate \
  --model models/onnx/model.onnx \
  --tokenizer models/onnx \
  --data data/eval.json \
  --output models/temperature.json

# Analyze calibration
python calibrate.py analyze \
  --model models/onnx/model.onnx \
  --tokenizer models/onnx \
  --data data/test.json \
  --temperature 1.5

# Test single input
python calibrate.py test-single \
  models/onnx/model.onnx \
  "find authenticate_user" \
  --tokenizer models/onnx \
  --temperature 1.5
```

**Outputs:**

```json
{
  "temperature": 0.4275,
  "metrics": {
    "accuracy": 0.9980,
    "ece_before": 0.1193,
    "ece_after": 0.0006,
    "mce_before": 0.5591,
    "mce_after": 0.4998
  },
  "calibration_samples": 8000,
  "intent_labels": ["SymbolQuery", "TextSearch", ...]
}
```

## Intent Classes

The classifier supports 8 intent classes (defined in `sqry-nl/src/types.rs`):

| Intent | Description | Example Queries |
|--------|-------------|-----------------|
| `SymbolQuery` | Find symbol definitions | "find authenticate_user", "where is UserAuth defined" |
| `TextSearch` | Grep/text search | "grep for TODO", "search for error messages" |
| `FindCallers` | Find function callers | "who calls login", "callers of authenticate" |
| `FindCallees` | Find function callees | "what does main call", "dependencies of handler" |
| `TracePath` | Trace call paths | "path from main to authenticate", "trace login to db" |
| `Visualize` | Generate diagrams | "visualize auth flow", "draw call graph" |
| `IndexStatus` | Check index status | "index status", "is index up to date" |
| `Ambiguous` | Unclear intent | "help", "hello", "???" |

## Acceptance Criteria

From FR-2026-001-nl-translation Step 11:

| AC | Requirement | Verified By |
|----|-------------|-------------|
| AC-11.1 | Balanced dataset | `generate_data.py` statistics output |
| AC-11.2 | >=1000 examples per intent | `--samples-per-intent 1000` |
| AC-11.3 | 10% manual verification | `generate_data.py verify` command |
| AC-11.4 | Negative examples | Ambiguous intent class |
| AC-11.5 | Valid ONNX model | `export_onnx.py verify` command |
| AC-11.6 | Int8 accuracy drop <2% | `export_onnx.py --eval` comparison |
| AC-11.7 | ECE <0.1 after calibration | `calibrate.py` ECE output |
| AC-11.8 | SHA256 checksums recorded | `checksums.json` output |

## Risk Mitigations

### H6: Training Data Quality

- Template-based generation (no LLM-generated data)
- Manual verification of 10% sample required
- Structured augmentation with known transformations
- All templates are human-authored and auditable

### C2: Model Size Constraint

- Base model: all-MiniLM-L6-v2 (22M params, 87MB full → 57MB INT8)
- INT8 dynamic quantization reduces model size by ~35%
- Accuracy drop verified to be <2%
- Quantization targets MatMul operations only

### C3: Confidence Calibration

- Temperature scaling applied post-hoc
- ECE (Expected Calibration Error) target: <0.1
- Reliability diagrams show calibration quality
- Temperature parameter stored separately for runtime use

## Directory Structure

```
training/
├── README.md              # This file
├── requirements.txt       # Python dependencies
├── generate_data.py       # Training data generation
├── train_classifier.py    # Model training
├── export_onnx.py         # ONNX export and quantization
├── calibrate.py           # Confidence calibration
├── data/                  # Generated data (gitignored)
│   ├── train.json
│   ├── eval.json
│   └── verify.json
└── models/                # Trained models (gitignored)
    ├── intent_classifier/
    ├── onnx/
    └── temperature.json
```

## Troubleshooting

### CUDA Out of Memory

Reduce batch size or disable FP16:

```bash
python train_classifier.py train --batch-size 16 --no-fp16 ...
```

### ONNX Export Fails

Ensure PyTorch and ONNX versions are compatible (see `requirements.txt` for pinned versions):

```bash
pip install -r requirements.txt
```

### Calibration ECE > 0.1

- Increase calibration dataset size
- Check for distribution shift between train/calibration data
- Try different temperature search bounds:

```python
optimal_temp = find_optimal_temperature(logits, labels, bounds=(0.5, 5.0))
```

### Quantization Accuracy Drop > 2%

- Try static quantization instead of dynamic
- Quantize fewer operator types
- Use larger calibration dataset for quantization

## Integration with sqry-nl

After training, copy the following files to `sqry-nl/models/`:

```bash
# Quantized ONNX model
cp models/onnx/quantized/model_quantized.onnx ../models/intent_classifier.onnx

# Model config and tokenizer
cp models/onnx/config.json ../models/
cp models/onnx/tokenizer.json ../models/

# Calibration parameters
cp models/temperature.json ../models/

# Checksums
cp models/onnx/checksums.json ../models/
```

The Rust runtime (`sqry-nl/src/classifier/`) loads these files at runtime.
BERT-architecture models (like MiniLM) require `token_type_ids` in addition to
`input_ids` and `attention_mask` — the Rust inference code handles this automatically.
