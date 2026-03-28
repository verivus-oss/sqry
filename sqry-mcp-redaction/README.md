# sqry-mcp-redaction

Client-side helper library for redacting sensitive data from MCP (Model Context Protocol) responses before sending them to external LLMs or cloud services.

## Overview

The sqry MCP server returns detailed code analysis results that may contain sensitive information including:

- **Absolute file paths** (exposing server structure)
- **Workspace root paths** (revealing internal infrastructure)
- **Source code context** (potentially proprietary code)
- **Documentation strings** (extracted comments)

This library provides configurable redaction to protect this data while preserving semantic information useful for code understanding.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
sqry-mcp-redaction = "1.31"
```

## Quick Start

```rust
use sqry_mcp_redaction::{Redactor, RedactionConfig};
use serde_json::json;

// Standard redaction (recommended for most cloud LLM integrations)
let redactor = Redactor::with_defaults();

let mut response = json!({
    "fileUri": "file:///home/user/project/src/main.rs",
    "workspace_path": "/home/user/project",
    "context": {
        "code": "fn main() { println!(\"Hello\"); }"
    }
});

let stats = redactor.redact(&mut response);

// Paths redacted, structure preserved
assert!(stats.workspace_path_redacted);
assert!(stats.paths_redacted > 0 || stats.uris_redacted > 0);
```

## Security Model

The library operates in **whitelist-first mode** by default:

- All fields are considered sensitive unless explicitly whitelisted
- Presets define which fields to preserve
- Unknown fields are redacted by default
- This provides fail-safe protection when MCP responses add new fields

## Presets

Four presets cover common deployment scenarios:

| Preset | Paths | Code | Docs | Use Case |
|--------|:-----:|:----:|:----:|----------|
| `none` | - | - | - | Trusted local tools only |
| `minimal` | Redact | Keep | Keep | Cloud LLMs needing code context |
| `standard` | Redact | Redact | Keep | Cloud LLMs, code confidential |
| `strict` | Hash | Redact | Redact | Untrusted external services |

### Using Presets

```rust
use sqry_mcp_redaction::RedactionConfig;

// No redaction (trusted environment)
let config = RedactionConfig::none();

// Redact paths only, preserve code
let config = RedactionConfig::minimal();

// Redact paths and code (default)
let config = RedactionConfig::standard();

// Maximum protection with filename hashing
let config = RedactionConfig::strict();
```

## Configuration Options

### Full Configuration Example

```rust
use sqry_mcp_redaction::RedactionConfig;
use std::path::PathBuf;

let config = RedactionConfig {
    // Workspace root for relative path conversion
    workspace_root: Some(PathBuf::from("/home/user/project")),

    // Path redaction
    redact_absolute_paths: true,
    redact_workspace_path: true,
    hash_filenames_in_strict: true,

    // Content redaction
    redact_code_context: true,
    redact_documentation: false,

    // Pattern detection (find paths in arbitrary strings)
    detect_paths_in_strings: true,

    // Custom field targeting
    custom_redact_fields: vec!["secret_token".to_string()],

    // JSONPath expressions for surgical redaction
    redact_paths: vec!["$..from.fileUri".to_string()],
    preserve_paths: vec!["$.metadata.name".to_string()],

    ..RedactionConfig::standard()
};
```

### Environment Variable Configuration

The library supports configuration via environment variables:

```bash
# Set redaction preset
export SQRY_REDACTION_PRESET=standard

# Set workspace root
export SQRY_WORKSPACE_ROOT=/home/user/project
```

```rust
// Load from environment
let config = RedactionConfig::from_env();
```

## API Reference

### Redactor

The main entry point for redaction operations.

```rust
use sqry_mcp_redaction::{Redactor, RedactionConfig};
use serde_json::Value;

// Create with default config
let redactor = Redactor::with_defaults();

// Create with custom config
let redactor = Redactor::new(config)?;

// Redact in place
let stats = redactor.redact(&mut json_value);

// Redact a clone (preserves original)
let (redacted, stats) = redactor.redact_clone(&json_value);

// Streaming redaction for large responses
let stats = redactor.redact_stream(reader, writer)?;

// Preview what would be redacted (dry-run)
let preview = redactor.preview(&json_value);
```

### RedactionResult

Statistics about what was redacted:

```rust
pub struct RedactionResult {
    /// Number of absolute paths redacted
    pub paths_redacted: usize,

    /// Number of file URIs redacted
    pub uris_redacted: usize,

    /// Whether workspace_path field was redacted
    pub workspace_path_redacted: bool,

    /// Number of code context fields redacted
    pub code_contexts_redacted: usize,

    /// Number of documentation fields redacted
    pub docs_redacted: usize,

    /// Number of paths found via pattern detection
    pub pattern_paths_redacted: usize,
}

// Check if anything was redacted
if stats.any_redacted() {
    println!("Redacted {} items", stats.total_redacted());
}
```

### Preview Mode

Inspect what would be redacted without modifying data:

```rust
let preview = redactor.preview(&json_value);

if preview.would_redact_anything() {
    println!("Would redact {} items:", preview.redaction_count());

    for target in &preview.targets {
        println!("  - {} at {}: {:?}",
            target.field_name,
            target.json_path,
            target.reason
        );
    }
}
```

## Path Handling

### Supported Path Formats

The library handles multiple path formats:

| Format | Example | Handling |
|--------|---------|----------|
| Unix absolute | `/home/user/file.rs` | Convert to relative or redact |
| Windows absolute | `C:\Users\file.rs` | Convert to relative or redact |
| File URIs | `file:///home/user/file.rs` | Parse, redact, preserve relative |
| UNC paths | `\\server\share\path` | Redact server/share, keep relative |

### Path Redaction Modes

```rust
// Convert to relative path (when workspace_root is set)
// file:///home/user/project/src/main.rs → src/main.rs

// Hash filename (strict mode)
// file:///home/user/project/src/main.rs → [a1b2c3d4]/src/main.rs

// Full replacement (when no workspace root)
// file:///home/user/project/src/main.rs → <redacted-path>
```

## JSONPath Expressions

Target specific nested fields using JSONPath syntax:

```rust
let config = RedactionConfig {
    // Redact all "from.fileUri" fields at any depth
    redact_paths: vec!["$..from.fileUri".to_string()],

    // Preserve specific metadata regardless of other rules
    preserve_paths: vec!["$.result.metadata".to_string()],

    ..RedactionConfig::minimal()
};
```

### Supported JSONPath Syntax

| Pattern | Matches |
|---------|---------|
| `$.field` | Root-level field |
| `$.a.b.c` | Nested path |
| `$..field` | Field at any depth (recursive descent) |
| `$[0]` | Array index |
| `$[*]` | All array elements |
| `$[0,1,2]` | Multiple indices |

## Pattern Detection

Find and redact paths embedded in arbitrary strings:

```rust
let config = RedactionConfig {
    detect_paths_in_strings: true,
    workspace_root: Some(PathBuf::from("/home/user/project")),
    ..RedactionConfig::minimal()
};

let redactor = Redactor::new(config)?;

let mut json = json!({
    "message": "Error at /home/user/project/src/main.rs:42 - syntax error"
});

redactor.redact(&mut json);

// message: "Error at src/main.rs:42 - syntax error"
// (absolute path converted to relative)
```

## Streaming Support

For large MCP responses, use streaming to avoid loading everything into memory:

```rust
use std::io::{BufReader, BufWriter};
use std::fs::File;

let redactor = Redactor::with_defaults();

let input = BufReader::new(File::open("response.json")?);
let output = BufWriter::new(File::create("redacted.json")?);

let stats = redactor.redact_stream(input, output)?;
```

## Error Handling

```rust
use sqry_mcp_redaction::{RedactionError, PathError};

match redactor.redact_stream(input, output) {
    Ok(stats) => println!("Redacted {} paths", stats.paths_redacted),
    Err(RedactionError::ParseError(e)) => eprintln!("Invalid JSON: {}", e),
    Err(RedactionError::StreamError(e)) => eprintln!("I/O error: {}", e),
    Err(RedactionError::ConfigError(msg)) => eprintln!("Bad config: {}", msg),
    Err(e) => eprintln!("Error: {}", e),
}
```

## Integration Examples

### With MCP Client

```rust
use sqry_mcp_redaction::{Redactor, RedactionConfig};

fn handle_mcp_response(response: &mut serde_json::Value) {
    let redactor = Redactor::with_defaults();
    let stats = redactor.redact(response);

    if stats.any_redacted() {
        log::info!("Redacted {} sensitive items before LLM submission",
            stats.total_redacted());
    }
}
```

### With Cloud LLM API

```rust
async fn query_llm(mcp_response: serde_json::Value) -> Result<String, Error> {
    let redactor = Redactor::new(RedactionConfig::standard())?;
    let (redacted, _stats) = redactor.redact_clone(&mcp_response);

    // Safe to send redacted response to external service
    let llm_response = external_llm_api::query(redacted).await?;
    Ok(llm_response)
}
```

### CI/CD Pipeline

```rust
// Maximum protection for logs
let config = RedactionConfig::strict();
let redactor = Redactor::new(config)?;

// Preview before committing to logs
let preview = redactor.preview(&response);
if preview.would_redact_anything() {
    log::debug!("Redacting {} items from pipeline output",
        preview.redaction_count());
}

let (redacted, _) = redactor.redact_clone(&response);
write_to_logs(&redacted);
```

## Performance

The library is designed for minimal overhead:

- **Sub-millisecond** processing for typical MCP responses
- **Single-pass** JSON traversal
- **No allocations** for non-redacted fields
- **Streaming** support for memory-constrained environments

## Thread Safety

`Redactor` is `Send + Sync` and can be shared across threads:

```rust
use std::sync::Arc;

let redactor = Arc::new(Redactor::with_defaults());

// Use from multiple threads
let redactor_clone = redactor.clone();
std::thread::spawn(move || {
    let mut response = get_response();
    redactor_clone.redact(&mut response);
});
```

## License

This project is licensed under the same terms as the sqry workspace (see root LICENSE file).

## Related Documentation

- [MCP Redaction Specification](../docs/development/mcp-redaction/01_SPEC_mcp_redaction.md) - Detailed specification
- [sqry MCP Server](../sqry-mcp/README.md) - The MCP server this library protects
