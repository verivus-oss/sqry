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

### Which values are paths

Path redaction changes only values that carry a path, never a name, an
enumerated value or a value's type:

- under a key that always names a path (`path`, `file_path`, `filePath`,
  `fileUri`, `file_uri`, `absolute_path`, `absolutePath`, and the workspace
  keys such as `root` and `workspace_path`), any non-empty string, read as
  one path (a relative one is read against the workspace);
- under `source`, `target`, `src`, `dst`, `uri` and `url`, which name a path
  in some responses and a symbol, an enumerated value or a web URL in
  others, any string that contains a path separator (`/` or `\`), unless it
  is one URL of a scheme other than `file` (`https://host/a/b`, with no
  whitespace in the value), which is scanned as any other string is (and
  in-string detection keeps such a URL's own path and redacts a host path
  in its query or parameters):
  - a value that, once its surrounding whitespace is trimmed, is exactly
    one absolute path (`/x`, `\x`, `C:\x`, `C:/x`, `\\server\share`, or a
    `file:` URI in any form: `file:///x`, `FILE:///x`, `file:/x`,
    `file://localhost/x`) is placed as a path key's value is;
  - any other value with a separator (a relative path such as
    `src/lib.rs`, a home path such as `~/x`, a drive-relative path such as
    `C:proj\lib.rs`, a path after a prefix such as `path:/x`, a path with
    whitespace in it, or prose such as `read from /mnt/x`) names no place,
    so it is never placed in the workspace: it is redacted as a path outside
    it, keeping only the text after its last separator (`<external>/lib.rs`),
    or a hash under `strict`;
  - a value with no separator (`alpha`, `crate::foo`, `a.b.c`,
    `static_estimate`, `C:cache`) is never changed;
- a null, boolean or number, never;
- in a list under any of these keys, each string, through any depth of
  nested lists, as the key's own string would be; an object in the list is
  read by its own keys.

A name that contains a separator cannot be told from a path, so under these
six keys it is redacted as one: a URL path such as `/api/users` (which
becomes `<external>/users`), an import specifier such as
`github.com/org/pkg`, a route such as `route::GET::/api/users`, an operator
such as `operator/`, or a PHP name written with `\`. sqry's own responses
put a symbol name under these keys only in `direct_callers` and
`direct_callees` (the symbol the caller sent) and in `semantic_diff` edges;
every other value they put there is an enumerated value with no separator.

Under `none`, nothing is rewritten unless a logical workspace with
exclusions is bound; then a path key's value, or a contextual value that is
exactly one absolute path, becomes `<excluded>/[hash]` when it lies under an
exclusion, and every other value passes unchanged.

## Presets

Five presets cover common deployment scenarios:

| Preset | Paths | Code | Docs | Use Case |
|--------|:-----:|:----:|:----:|----------|
| `none` | - | - | - | Trusted local tools only |
| `minimal` | Redact | Keep | Keep | Cloud LLMs needing code context |
| `relative` | Redact (clean workspace-relative layout) | Keep | Keep | As `minimal`, with no source-root prefix |
| `standard` | Redact | Redact | Keep | Cloud LLMs, code confidential |
| `strict` | Hash | Redact | Redact | Untrusted external services |

A preset name is read by `RedactionPreset::parse`: trimmed, in any letter
case (`Strict`, ` minimal `). Any other value is no preset. What a surface
does with one is its own, and none reads it as "no redaction": `sqry-mcp`
and `sqryd` refuse to start, naming the value, and
`RedactionConfig::from_env` and `from_preset_with_env` read it as
`standard` with a warning.

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

The full set of environment overrides (read by `RedactionConfig::from_env` /
`from_preset_with_env` in `sqry-mcp-redaction/src/config.rs`):

| Variable | Default | Purpose |
|----------|---------|---------|
| `SQRY_REDACTION_PRESET` | `standard` | Selects base preset: `none`, `minimal`, `relative`, `standard`, `strict`, trimmed and in any letter case. Here an unknown value falls back to `standard` with a warning; `sqry-mcp` and `sqryd` (whose default is `minimal`) refuse to start on one instead. |
| `SQRY_REDACTION_MAX_DEPTH` | `128` | Maximum walker recursion depth for nested JSON. Clamped to `[8, 512]`. Values nested beyond this are dropped to prevent stack-overflow attacks. |
| `SQRY_REDACT_PATHS` | preset value (`true` for `minimal`/`standard`/`strict`) | Toggles redaction of absolute filesystem paths (`/home/...`, `C:\...`). `1`/`true` enables, `0`/`false` disables. |
| `SQRY_REDACT_WORKSPACE` | preset value (`true` for `minimal`/`standard`/`strict`) | Toggles redaction of the `workspace_path` / workspace-root field. |
| `SQRY_REDACT_URIS` | preset value (`true` for `minimal`/`standard`/`strict`) | Toggles redaction of `file://` URIs. |
| `SQRY_REDACT_CODE` | preset value (`false` for `minimal`, `true` for `standard`/`strict`) | Toggles redaction of `code` / source-snippet context fields. |
| `SQRY_REDACT_DOCS` | preset value (`false` for `minimal`/`standard`, `true` for `strict`) | Toggles redaction of documentation/comment string fields. |
| `SQRY_REDACT_PATTERNS` | preset value (`true` for `minimal`/`standard`/`strict`) | Toggles pattern detection of paths embedded inside arbitrary string values. |
| `SQRY_HASH_FILENAMES` | preset value (`true` only for `strict`) | When enabled, replaces filenames with a salted hash (e.g. `[a1b2c3d4]/src/main.rs`) instead of full redaction. |
| `SQRY_HASH_SALT` | unset | Salt string mixed into filename hashing. Empty string normalized to `None`. Length capped at 256 chars. |
| `SQRY_WORKSPACE_ROOT` | unset | Absolute path used to convert absolute paths into workspace-relative form before redaction. |
| `SQRY_WHITELIST_FIELDS` | preset list | Comma-separated extra field names to add to the whitelist (these fields are preserved, not redacted). Appended to the preset's built-in whitelist. |
| `SQRY_PRESERVE_PATHS` | unset | Comma-separated JSONPath expressions; matched values are preserved verbatim regardless of other rules. |

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

Detection finds an absolute path in a string whatever directory it starts
in (`/dev/shm/x`, `/nix/store/x`, `/mnt/x`, not a fixed list of prefixes),
within the limits below:

- a `file:` URI in any letter case (`file:///x`, `FILE:///x`, `file:/x`,
  `file://host/x`, and the `file:` URI of `git+file:///x`);
- a UNC path whose server name is letters, digits, `_`, `.` and `-`
  (`\\server\share\x`), also as `Debug` writes it with every
  backslash doubled (`\\\\server\\share\\x`), and a Windows drive path
  (`C:\x`, `c:/x`, `C:\\x`);
- a Unix absolute path (`/x`, `//server/share`): a `/` followed by a path
  character, at the start of the text or after whitespace, punctuation, an
  operator or a shell redirection (`=`, `:`, `,`, `;`, `(`, `|`, `&`, `+`,
  `%`, `@`, `$`, `#`, `*`, `!`, `?`, `^`, `>`, `<`, a quote:
  `2>/dev/shm/log`, `a&/home/u/x`, `path::/x`; not `-`, `_`, `.` or `~`,
  which continue a word), after a one-letter flag
  opening a token (`-I/usr/include`), after an ellipsis (`see.../x`), and
  after a `~` that follows a word (`x~/x`); also a Unix path whose slashes
  are all JSON-escaped and whose first segment follows `\/` directly
  (`\/home\/u\/s`; `\/\/srv` is not found, see the limits).

An escape written out in text by `Debug`, JSON or C is a boundary before
every one of these forms, as whitespace is: an unescaped backslash, then
`n`, `t`, `r`, `0`, `f`, `v`, `b`, `a` or `e`, `x` and two hex digits, `u`
and four hex digits, or `u{`, one to six hex digits and `}`
(`line\nC:\x`, `line\u000afile:///x`, `line\x0a/x`).

A path ends at whitespace, a quote, a backtick, an angle bracket, a closing
bracket it did not open (`/a/My(1)/b` is one path), or an unescaped
backslash where an escape really begins (the second backslash of a `\\`
pair never begins one, so a pair is never split): `\"` or `\'` for every
path (so `Debug`'s `\"/home/u/s\"` keeps its closing `\"`); for a UNC path
as `Debug` writes it, any written escape; for a Unix path, `\n`, `\r` or
`\t` whatever follows (`/x/secret.rs\nhint`), `\\` that no path character
follows, and any other written escape that the end, a character that
cannot continue a path, or a `/` follows. Any other backslash stays in a
Unix path and is redacted with it (`/mnt/c/Users\alice\secret.txt`,
`/home/u/s\backup/x`, `/home/u/a\xyz/x`, `Debug`'s `/home/alice\\t/x`). A
Windows or raw UNC path keeps all its own backslashes.
A URL of a scheme other than `file` keeps its scheme, authority and path,
and its query, parameters and fragment are scanned as other text is
(`ws://h/a?f=/home/u/z` redacts `/home/u/z`). These are left as they are:

- a URL's own path (`https://host/a/b`), also after an escape;
- a relative path (`src/lib.rs`, `./x`, `~/x` with `~` opening a token,
  `$HOME/x`);
- an endpoint's qualified name exactly as sqry's plugins write it,
  `route::<METHOD>::/path` with `route` opening a token and one of `GET`,
  `POST`, `PUT`, `DELETE`, `PATCH`, `HEAD`, `OPTIONS`, `ALL`
  (`Foo::BAR::/x` is redacted);
- a URI fragment or JSON pointer after a word, a `/` or a quote
  (`schema.json#/defs/x`, and `"$ref": "#/definitions/X"` in a code
  snippet, which `minimal` and `relative` keep);
- a glob (`**/x`), `c++/x`, a scoped package (`@scope/pkg`), a placeholder
  (`<workspace>/x`), a closing markup tag (`</div>`) and a comment
  delimiter (`// x`, `/* x */`);
- a sqry regular-expression literal after `~=` (`name~=/.*/i`) when nothing
  after it can continue into a path: no whitespace before its closing `/`,
  then only flags (`i`, `m`, `s`), then the end of the text, whitespace, a
  quote, a backtick, one of `)`, `]`, `}`, `<`, `>`, `,`, `;`, `:`, `!`,
  `?`, or a `.` that the end, whitespace, a quote, a backtick or a closing
  bracket follows. Any other literal (`name~=/foo`,
  `name~=/x/./home/u/s`) fails safe and is scanned as other text is, so it
  never hides a later path;
- a regex between slashes elsewhere whose first character cannot open a
  path segment (`/^a+$/i`, `/[a-z]+/`).

Free-text detection is heuristic and incomplete: it scans text for the
shapes a path takes, and cannot see every way a path can be written. A
structural redesign (paths emitted through a typed formatter, so redaction
is exact rather than a text scan) is tracked separately as
verivus-oss/sqry#908; until then the limits below are frozen, and each is
pinned by a test that asserts today's behaviour (the exact output, or for the
over-redaction, whitespace and glued-word groups the exact path detected), so
any change in what
stays readable is visible.

The limits, with what stays readable (under a path key the whole value is
one path, so none of these applies there):

- Over-redaction: a token that only reads as an absolute path is redacted
  (`/results/0`, `#/definitions/x` after whitespace or at the start of the
  text, the `/foo` of `name~=/foo`, `/api/users`, `?next=/api/users`,
  `@/components/x`, `/foo/i` outside `~=`, `/2`); only its last segment
  stays readable.
- Whitespace inside a path: the path is found only up to its first
  whitespace; `/mnt/My Dir/x` leaves `Dir/x` readable.
- A path glued to a word longer than a one-letter flag, or after a backslash
  that begins no escape: not found, wholly readable (`-isystem/usr/include`,
  `1/home/u/x`, `a\q/x`, `x\\n/x`, `x\\nC:\x`, a short `\x0` or `\u00a`,
  `a\\/home/u/x`).
- A backslash-separated segment that is exactly an escape (`\b`, `\t`, `\n`
  and the other letter escapes, `\xNN`, `\uNNNN`) followed by `/` or `\`,
  and any `\n`, `\r` or `\t` in a Unix path: the path ends there and what
  follows stays readable (`/home/alice\t/secret` and
  `/mnt/c/proj\b/secret.txt` keep `alice` and `proj` as file names;
  `/mnt/c/Users\t\alice\secret.txt` leaves `\alice\secret.txt`;
  `/home/alice\\\\secret` leaves `\\\\secret`; `/mnt/c/x\notes\a` leaves
  `\notes\a`).
- JSON-escaped `//` (`\/\/srv\/share\/secret`,
  `file:\/\/\/home\/alice\/secret`): not found, wholly readable.
- The octal escape `\012` is no boundary: `line\012/home/alice/secret` is
  not found, wholly readable.
- A doubly escaped UNC path (`\\\\\\\\srv\\\\share`) and a raw
  `\\?\UNC\srv\share\secret`: not found, wholly readable.
- A UNC path's output repeats its `<network:hash>` prefix
  (`<network:H><network:H>/x`); the hash, never the server, is readable.
- A `Debug` drive path runs on through a written escape:
  `"C:\\x\\y\nnext"` becomes `"<external>/nnext"`, the text after the
  escape swallowed into the file name.
- A `/` right after a `-` is no token start, since `-` continues a word
  (`a-b/c`): a path after a `-` that opens a token or ends a word is not
  found, wholly readable (`-/home/alice/old.rs`, `--root=-/home/alice/x`,
  `1-/home/alice/x`).
- A UNC path whose server name holds any character but a letter, a digit,
  `_`, `.` or `-` is not found in text, wholly readable
  (`\\wsl$\Ubuntu\home\alice\x`, the WSL share); under a path key the same
  value is redacted as any UNC path is, the server hashed and the
  directories after the share kept (`<network:H><network:H>/home/alice/x`).

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
