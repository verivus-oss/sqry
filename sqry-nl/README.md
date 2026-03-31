# sqry-nl

Natural language to sqry query translation layer.

## Overview

`sqry-nl` translates natural language queries like "find authentication functions in rust" into executable sqry commands like `sqry query "name~=/auth/ AND kind:function" --language rust`.

The crate provides a complete translation pipeline:

```
Input: "find authentication functions"
       ↓
Preprocess: Unicode normalization, homoglyph detection
       ↓
Extract: symbols=["authentication"], kind=function
       ↓
Classify: Intent::SymbolQuery (0.85 confidence)
       ↓
Assemble: sqry query "name~=/authentication/ AND kind:function"
       ↓
Validate: whitelist check, safety validation
       ↓
Output: TranslationResponse::Execute { command, confidence, ... }
```

## Features

- **6-stage translation pipeline**: preprocess → extract → classify → assemble → validate → cache
- **4-tier response system**: Execute (≥85%) / Confirm (≥65%) / Disambiguate (<65%) / Reject
- **Strong safety guarantees**: Whitelist-only commands, metachar rejection, path traversal protection
- **LRU caching**: Context-aware cache keys for repeated queries
- **Compact classifier**: all-MiniLM-L6-v2 (22M params, 57MB INT8) with 99.75% accuracy
- **Feature-gated classifier**: Works without ONNX via rule-based fallback

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
sqry-nl = "0.1"
```

### Feature Flags

| Feature | Default | Description |
|---------|---------|-------------|
| `classifier` | Yes | Enable MiniLM-L6-v2 ONNX classifier (requires ONNX Runtime) |

To use without ONNX (rule-based fallback only):

```toml
[dependencies]
sqry-nl = { version = "0.1", default-features = false }
```

## Quick Start

```rust
use sqry_nl::{Translator, TranslatorConfig, TranslationResponse};

fn main() -> anyhow::Result<()> {
    let config = TranslatorConfig::default();
    let mut translator = Translator::new(config)?;

    match translator.translate("find authentication functions in rust") {
        TranslationResponse::Execute { command, confidence, .. } => {
            println!("Execute: {} ({:.0}% confidence)", command, confidence * 100.0);
            // Run the command
        }
        TranslationResponse::Confirm { command, prompt, .. } => {
            println!("{}", prompt);
            // Ask user for confirmation
        }
        TranslationResponse::Disambiguate { options, prompt } => {
            println!("{}", prompt);
            for opt in options {
                println!("  - {}: {}", opt.description, opt.command);
            }
            // Let user choose
        }
        TranslationResponse::Reject { reason, suggestions } => {
            eprintln!("Cannot translate: {}", reason);
            for suggestion in suggestions {
                eprintln!("  Suggestion: {}", suggestion);
            }
        }
    }

    Ok(())
}
```

## Supported Intents

| Intent | Example Query | Generated Command |
|--------|---------------|-------------------|
| `SymbolQuery` | "find login function" | `sqry query "login" --kind function` |
| `TextSearch` | "grep for TODO" | `sqry search "TODO"` |
| `FindCallers` | "who calls authenticate" | `sqry graph direct-callers "authenticate"` |
| `FindCallees` | "what does main call" | `sqry graph direct-callees "main"` |
| `TracePath` | "trace from main to db" | `sqry graph trace-path "main" "db"` |
| `Visualize` | "draw call graph" | `sqry visualize --format mermaid` |
| `IndexStatus` | "index status" | `sqry index --status` |
| `Ambiguous` | "help" | (disambiguation options) |

## Configuration

```rust
use sqry_nl::{TranslatorConfig, CacheConfig};

let config = TranslatorConfig {
    // Model directory (for ONNX classifier)
    model_dir: Some("/path/to/models".to_string()),

    // Working directory for relative paths
    working_directory: Some("/my/project".to_string()),

    // Confidence thresholds
    execute_threshold: 0.85,  // Auto-execute above this
    confirm_threshold: 0.65,  // Ask confirmation above this

    // Cache configuration
    cache_config: Some(CacheConfig {
        capacity: 128,
        ttl: None,  // No expiration
    }),

    // Default result limit
    default_limit: 100,

    // Language filters
    languages: vec!["rust".to_string()],
};
```

## Safety

All generated commands are validated against strict safety rules:

| Check | Description |
|-------|-------------|
| **Whitelist** | Commands must match allowed templates |
| **Metacharacters** | Rejects `;`, `|`, `&`, `$`, backticks, etc. |
| **Environment variables** | Rejects `$HOME`, `${VAR}`, etc. |
| **Path traversal** | Rejects `..`, absolute paths |
| **Write operations** | Rejects `--force`, `repair`, `prune` |
| **Length limit** | Commands capped at 4KB |

## Architecture

```
sqry-nl/
├── src/
│   ├── lib.rs           # Public API
│   ├── types.rs         # Core types (Intent, TranslationResponse, etc.)
│   ├── translator.rs    # Main Translator API
│   ├── preprocess/      # Unicode normalization, homoglyph detection
│   ├── extractor/       # Entity extraction (symbols, languages, etc.)
│   ├── classifier/      # Intent classification (ONNX + fallback)
│   ├── assembler/       # Template-based command generation
│   ├── validator/       # Safety validation
│   ├── cache/           # LRU translation cache
│   └── error.rs         # Error types
├── training/            # Python training pipeline
│   ├── generate_data.py # Training data generation
│   ├── train_classifier.py
│   ├── export_onnx.py
│   └── calibrate.py
├── benches/
│   └── eval_harness.rs  # Accuracy & latency benchmarks
└── tests/
    └── golden_queries.toml  # 120+ test queries
```

## API Reference

### Types

```rust
// Intent classification result
pub enum Intent {
    SymbolQuery,
    TextSearch,
    TracePath,
    FindCallers,
    FindCallees,
    Visualize,
    IndexStatus,
    Ambiguous,
}

// Translation response with confidence tiers
pub enum TranslationResponse {
    Execute { command, confidence, intent, cached, latency_ms },
    Confirm { command, confidence, prompt },
    Disambiguate { options, prompt },
    Reject { reason, suggestions },
}

// Extracted entities from natural language
pub struct ExtractedEntities {
    pub symbols: Vec<String>,
    pub languages: Vec<String>,
    pub paths: Vec<String>,
    pub kind: Option<SymbolKind>,
    pub limit: Option<u32>,
    pub depth: Option<u32>,
    // ...
}
```

### Translator

```rust
impl Translator {
    /// Create a new translator with configuration
    pub fn new(config: TranslatorConfig) -> NlResult<Self>;

    /// Create with default configuration
    pub fn load_default() -> NlResult<Self>;

    /// Translate natural language to sqry command
    pub fn translate(&mut self, input: &str) -> TranslationResponse;

    /// Get translation count
    pub fn translation_count(&self) -> u64;

    /// Get cache statistics
    pub fn cache_stats(&self) -> Option<CacheStats>;

    /// Clear the translation cache
    pub fn clear_cache(&self);
}
```

## Model

The intent classifier uses [all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2) (22M params) fine-tuned for 8 intent classes, exported to ONNX with INT8 dynamic quantization.

| Metric | Value |
|--------|-------|
| Base model | `sentence-transformers/all-MiniLM-L6-v2` |
| Parameters | 22M |
| ONNX INT8 size | 57MB |
| Accuracy | 99.75% |
| P50 latency | 2.1ms |
| P90 latency | 3.0ms |
| Calibrated ECE | 0.0006 |

## Benchmarks

Run the evaluation harness:

```bash
cargo bench --bench eval_harness
```

Target metrics:
- Intent accuracy: ≥95% (with trained model), ≥70% (rule-based fallback)
- Command accuracy: ≥85%
- P95 latency: <100ms (without model), <500ms (with model)

## Training

To train the classifier:

```bash
cd training
pip install -r requirements.txt

# 1. Generate training data (1000+ samples/intent required)
python generate_data.py generate --output data/train.json --samples-per-intent 1000

# 2. Generate evaluation data (separate seed)
python generate_data.py generate --output data/eval.json --samples-per-intent 1000 --seed 42

# 3. Train model (default: all-MiniLM-L6-v2)
python train_classifier.py train \
  --train data/train.json --eval data/eval.json \
  --output models/intent_classifier \
  --model sentence-transformers/all-MiniLM-L6-v2

# 4. Export to ONNX with INT8 quantization
python export_onnx.py export \
  --model models/intent_classifier/final \
  --output models/onnx --quantize --eval data/eval.json

# 5. Calibrate confidence (temperature scaling)
python calibrate.py calibrate \
  --model models/onnx/quantized/model_quantized.onnx \
  --tokenizer models/onnx --data data/eval.json \
  --output models/temperature.json

# 6. Deploy to sqry-nl/models/
cp models/onnx/quantized/model_quantized.onnx ../models/intent_classifier.onnx
cp models/onnx/{config.json,tokenizer.json,checksums.json} ../models/
cp models/temperature.json ../models/
```

See [training/README.md](training/README.md) for detailed instructions.

## License

MIT License - see [LICENSE](../LICENSE) for details.

## Related

- [sqry](../) - Semantic code search CLI
- [sqry-mcp](../sqry-mcp/) - MCP server with `sqry_ask` tool
- [sqry-openai](../sqry-openai/) - OpenAI Agents SDK integration
