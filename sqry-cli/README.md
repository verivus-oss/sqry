# sqry CLI

> Semantic code search tool that understands code structure through AST analysis.

## Overview

`sqry` is a command-line tool for searching code by **what it means**, not just what it says. It uses Abstract Syntax Tree (AST) analysis to understand code structure and find symbols with precision.

## Installation

### Install from Source (Recommended)

```bash
# Clone the repository
git clone https://github.com/<ORG>/<REPO>
cd sqry

# Install to cargo bin (makes sqry available system-wide)
cargo install --path sqry-cli

# Verify installation
sqry --version
```

The `sqry` binary will be installed to `~/.cargo/bin/sqry` and will be available system-wide if `~/.cargo/bin` is in your PATH.

### Build Only (Without Installing)

```bash
# Build release binary
cargo build --release --package sqry-cli

# The binary will be at: target/release/sqry
./target/release/sqry --help
```

### Check Installation

```bash
# Verify sqry is in PATH
which sqry

# Check version
sqry --version

# Test it
sqry main src/
```

## Quick Start

```bash
# Search for 'main' in current directory
sqry main

# Search in specific directory
sqry test src/

# Search for functions only
sqry handle --kind function

# Get JSON output
sqry main --json
```

## Usage

### Basic Search

```bash
sqry [OPTIONS] <PATTERN> [PATH]
```

**Examples:**

```bash
# Find all occurrences of "parse"
sqry parse

# Search for "handler" in src/ directory
sqry handler src/

# Case-insensitive search
sqry error --ignore-case

# Exact match (disable regex)
sqry "MyClass" --exact
```

### Search Command (Explicit)

```bash
sqry search [OPTIONS] <PATTERN> [PATH]
```

Same as basic search, but explicit. Useful in scripts:

```bash
sqry search "handle.*" src/ --kind function
```

### Query Command

```bash
sqry query [OPTIONS] <QUERY> [PATH]
```

AST-aware queries using predicates for semantic code search:

**Query Syntax:**
- `kind:<type>` - Filter by symbol type (function, class, struct, etc.)
- `name:<value>` - Filter by exact name (`name~=/.../` for regex)
- `lang:<language>` - Filter by programming language
- `path:<glob>` - Filter by file path (glob pattern; `file:` is an alias, `path~=/.../` for regex)
- `parent:<value>` - Filter by parent symbol (`parent~=/.../` for regex)

Boolean operators (AND/OR/NOT) are required between predicates.

**Examples:**

```bash
# Find all functions
sqry query "kind:function"

# Find functions with 'test' in name
sqry query "kind:function AND name~=/test/"

# Find Rust functions
sqry query "kind:function AND lang:rust"

# Find methods in specific class
sqry query "kind:method AND parent:MyClass"

# Complex query
sqry query "kind:function AND name~=/^handle/ AND lang:rust"

# Explain query (don't execute)
sqry query "kind:function" --explain
```

### Session Shell (Keep the Cache Warm)

Use the interactive shell when you want to run many queries without paying the cold-start cost each time.

```bash
sqry shell /path/to/repo                 # Starts a REPL with warm session cache
sqry shell /path/to/repo --limit 50      # Limit results while exploring
sqry shell /path/to/repo --json          # Emit JSON-formatted results
```

Inside the shell:
- Enter semantic queries as you would with `sqry query`
- Type `stats` for cache metrics, `refresh` to reload the index, `history` to inspect previous queries, and `exit` to quit

### Batch Command

Drive multiple queries from a file while keeping the `.sqry-index` warm.

```bash
sqry batch /path/to/repo --queries queries.txt --stats
sqry batch /path/to/repo --queries queries.txt --output jsonl
sqry batch /path/to/repo --queries queries.txt --output csv --continue-on-error
```

Each non-empty line in `queries.txt` is executed in order; use `--stats` for a summary block and `--output` to pick between text, JSON, JSONL, or CSV.

### Query Session Flag

Need a warm cache inside a single invocation (for shell scripts or CI hooks)? Add `--session` to `sqry query`:

```bash
sqry query --session "kind:function" /repo
sqry query --session "kind:class" /repo   # Reuses the session manager initialized above
```

Requirements and tips:
- `.sqry-index` must exist; otherwise the command reports a helpful error
- The warm cache only lives for the current process—use `sqry shell` or `sqry batch` for long-lived sessions
- `repo:` predicates remain reserved for `sqry workspace query` (multi-repo searches)

**Supported Symbol Types:**
- `function` or `fn`
- `class`
- `method`
- `struct`
- `enum`
- `interface` or `trait`
- `variable` or `var`
- `constant` or `const`
- `type`
- `module` or `mod`
- `namespace` or `ns`

**Supported Languages:**
- `rust`
- `javascript`
- `typescript`
- `python`
- `go`

## Workspace (Multi-Repo) Management

Manage multiple repositories under a single workspace root:

```bash
sqry workspace init /projects --name "My Workspace"
sqry workspace scan /projects --mode git-roots
sqry workspace add /projects /projects/backend --name backend
sqry workspace remove /projects backend
sqry workspace query /projects "kind:function AND repo:backend"
sqry workspace stats /projects
```

Discovery modes:
- `index-files` (default): find `.sqry-index` files anywhere under the root
- `git-roots`: require a `.git/` directory plus `.sqry-index`

## Advanced Analysis Commands

Standalone analysis commands powered by the unified graph:

```bash
sqry duplicates --type body --threshold 90
sqry cycles --type imports --min-depth 3
sqry unused --scope public --lang rust
sqry explain src/main.rs main
sqry similar src/lib.rs process_data --threshold 0.8
sqry subgraph main --depth 3 --include-imports
sqry impact authenticate --depth 5 --show-files
sqry diff main feature-branch --kind function --change-type added
sqry hier "kind:function AND name:parse" --max-files 10 --context 5
sqry export --format mermaid --filter-lang rust,go --output graph.mmd
```

## Aliases, History, and Insights

Persist frequently used queries and track local usage insights:

```bash
sqry query "kind:function AND name:test" --save-as test-funcs
sqry alias list --global
sqry alias export aliases.json
sqry history list --limit 50
sqry insights show
sqry insights config --disable
```

Notes:
- Aliases are stored locally (project) or globally (`~/.config/sqry/`).
- History entries automatically redact common secrets.
- Insights are stored locally only; no network calls are made.

## Diagnostics & Troubleshooting

Generate a sanitized diagnostics bundle for support:

```bash
sqry troubleshoot --dry-run
sqry troubleshoot -o bundle.json --include-trace
```

The bundle omits code content, file paths, and secrets by default.

## Output UX Options

```bash
sqry --pager query "kind:function"
sqry --no-pager search "error" --json
sqry --pager-cmd "less -R" query "name:main"
sqry --theme light --sort name query "kind:function"
```

## Filtering Options

### By Symbol Type (`--kind`)

Filter results by symbol type:

```bash
sqry main --kind function     # Functions only
sqry User --kind class        # Classes only
sqry API --kind struct        # Structs only
```

**Supported types:**
- `function` - Functions and function declarations
- `class` - Classes
- `method` - Methods (class/impl methods)
- `struct` - Struct definitions
- `enum` - Enum definitions
- `interface` - Interfaces (TypeScript, Go)
- `trait` - Traits (Rust)
- `variable` - Variables
- `constant` - Constants
- `type` - Type aliases
- `module` - Modules
- `namespace` - Namespaces

### By Language (`--lang`)

Filter by programming language:

```bash
sqry main --lang rust         # Rust files only
sqry handler --lang javascript # JavaScript files
sqry parse --lang python      # Python files
```

**Supported languages:**
- `rust` (.rs)
- `javascript` (.js, .jsx)
- `typescript` (.ts, .tsx)
- `python` (.py)
- `go` (.go)

### Pattern Matching

```bash
# Regex pattern (default)
sqry "handle.*"               # Matches handle, handler, handleRequest, etc.

# Exact match
sqry "MyClass" --exact        # Only matches exact string "MyClass"

# Case-insensitive
sqry error --ignore-case      # Matches Error, error, ERROR, etc.
```

## Output Formats

### Text Output (Default)

Human-readable format with colors:

```bash
sqry main
```

**Output:**
```
src/main.rs:13:0: function main
tests/test.rs:10:1: function test_main

2 matches found
```

Colors (when TTY detected):
- **Green**: File path
- **Blue**: Line:column
- **Yellow**: Symbol type
- **Bold**: Symbol name

### JSON Output

Machine-readable JSON format:

```bash
sqry main --json
```

**Output:**
```json
[
  {
    "name": "main",
    "kind": "function",
    "file_path": "src/main.rs",
    "start_line": 13,
    "start_column": 0,
    "end_line": 48,
    "end_column": 1
  }
]
```

### Count Only

Show only the number of matches:

```bash
sqry test --count
```

**Output:**
```
15 matches found
```

## Search Options

### Directory Traversal

```bash
# Maximum directory depth
sqry main --max-depth 3

# Include hidden files/directories
sqry config --hidden

# Follow symlinks
sqry lib --follow
```

### File Discovery

By default, `sqry` searches these file types:
- Rust: `.rs`
- JavaScript: `.js`, `.jsx`
- TypeScript: `.ts`, `.tsx`
- Python: `.py`
- Go: `.go`

Respects `.gitignore` patterns automatically.

## Environment Variables

### `NO_COLOR`

Disable colored output:

```bash
export NO_COLOR=1
sqry main  # No colors
```

Or use the flag:

```bash
sqry main --no-color
```

## Examples

### Find All Functions

```bash
sqry ".*" --kind function
```

### Find Test Functions

```bash
sqry "test.*" --kind function
```

### Find Error Handlers

```bash
sqry ".*error.*" --ignore-case --kind function
```

### Search Specific Package

```bash
sqry Handler src/api/ --kind class
```

### Export Results to File

```bash
sqry main --json > results.json
```

### Combine with Other Tools

```bash
# Count matches per file
sqry error --json | jq '.[] | .file_path' | sort | uniq -c

# Extract just names
sqry "parse.*" --json | jq -r '.[].name'

# Filter by line number
sqry main | awk -F: '$2 > 100'
```

## Exit Codes

- `0` - Success (matches found)
- `1` - Error or no matches found

## Limitations

**Current limitations:**

1. **SearchMode Variants**: Only `Text` and `Regex` modes are currently implemented
   - `SearchMode::Semantic` and `SearchMode::Fuzzy` are not yet available
   - Attempting to use unimplemented modes will return a clear error message
   - **When using the API**: Only use `SearchMode::Text` (literal text search) or `SearchMode::Regex` (pattern matching)
   - Future releases will add semantic search (AST-aware) and fuzzy matching

2. **Query Command**: Fully functional with comprehensive predicate support
   - Supports `kind:`, `name~=`, `lang:`, `file:`, `parent:`, `in:`, `depth:`, `path:`
   - Boolean operators: AND, OR, NOT with parentheses
   - Index-aware for instant results (when `.sqry-index` exists)

3. **Index Management**: Persistent indexing available via `sqry index` command
   - Run `sqry index` to create persistent `.sqry-index` file
   - Run `sqry update` for incremental updates (only changed files)
   - Provides 40x-200x speedup for queries on indexed projects

4. **Limited Languages**: 5 languages currently supported
   - Rust, JavaScript, TypeScript, Python, Go
   - Plugin system allows extension (see Plugin Development Guide)

## Performance Tips

1. **Limit search scope:**
   ```bash
   sqry main src/          # Search only src/
   sqry main --max-depth 2 # Limit depth
   ```

2. **Use exact match when possible:**
   ```bash
   sqry "MyClass" --exact  # Faster than regex
   ```

3. **Filter by language:**
   ```bash
   sqry parse --lang rust  # Skip other file types
   ```

## Troubleshooting

### No matches found

- Check pattern spelling
- Try case-insensitive: `--ignore-case`
- Try without type filter: remove `--kind`
- Verify files exist: `ls -la <path>`

### Too many results

- Add type filter: `--kind function`
- Add language filter: `--lang rust`
- Use more specific pattern: `sqry "exact_name" --exact`

### SearchMode API errors

If you're using the `sqry-core` library directly and encounter:
```
SearchMode::Semantic is not yet implemented. Please use SearchMode::Text or SearchMode::Regex instead.
```

**Solution**: Use `SearchMode::Text` for literal text matching or `SearchMode::Regex` for pattern matching:

```rust
use sqry_core::search::{SearchConfig, SearchMode, Searcher};

let config = SearchConfig {
    mode: SearchMode::Regex,  // or SearchMode::Text
    ..Default::default()
};
```

## Development

### Build

```bash
cargo build --package sqry-cli
```

### Test

```bash
cargo test --package sqry-cli
```

### Run

```bash
cargo run --package sqry-cli -- main src/
```

## Related Documentation

- [Core Library](../sqry-core/README.md) - Core AST and symbol extraction
- [Plugin System](../PLUGIN_ARCHITECTURE.md) - Language plugin architecture
- [Implementation Plan](../IMPLEMENTATION_PLAN.md) - Development roadmap

## License

See [LICENSE](../LICENSE) file.

## Version

**Current**: v3.1.0
**Status**: See [CHANGELOG.md](../CHANGELOG.md) for release status

See [CHANGELOG.md](../CHANGELOG.md) for version history.
