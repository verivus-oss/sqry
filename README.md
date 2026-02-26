# sqry - Semantic Query for Code

**Search code by what it means, not just what it says.**

sqry is a blazing-fast semantic code search tool that understands code structure, not just text patterns. Find functions, classes, and symbols with ease using AST-aware queries and cross-file semantic analysis.

Website: https://sqry.dev

> 📅 **Current Version**: v4.4.2
> ✅ **35 Languages**: Rust, Python, TypeScript, JavaScript, Go, Java, C/C++, C#, Kotlin, Ruby, Swift, Scala, Lua, R, Dart, PHP, SQL, Shell, Haskell, Perl, Elixir, Vue, Svelte, Groovy, Zig, HTML, CSS, Terraform, Puppet, Pulumi, and more
> ✅ **28 with Full Relation Support (Tier 1)**: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig - complete call/import/export relation extraction
> ✅ **80% Tier 1 Coverage**: 28 of 35 supported languages with full relation extraction (callers, callees, imports, exports)
> ✅ **Graph Architecture**: 5-10x faster code analysis with unified graph mode (default)
> ✅ **Pass 5 Cross-Language Detection**: FFI linking (Rust↔C/C++), HTTP linking (JS/TS↔Python/Java/Go), SQL table access, Dart MethodChannel, Flutter widget hierarchies
> ✅ **MCP Server**: 33 JSON-RPC tools for AI assistants (Claude, Codex, Gemini, Cursor, Windsurf) with Layer 2 on-demand documentation resources
> ✅ **Language Server Protocol**: `sqry lsp` serves hover/definition/references, call hierarchy, code actions, and 27 custom sqry methods for VSCode, Neovim, Helix, and any LSP 3.17 client
> ✅ **Multi-Workspace Cache Isolation**: GraphIdentity-based cache keys, LRU eviction, TOCTOU-safe freshness checks
> ✅ **Graph Queries**: Path tracing, call-chain depth, dependency trees, complexity metrics, cycle detection
> ✅ **Visualization**: Export as DOT, Mermaid, D2, or JSON diagrams
> ✅ **Multiprocess Cache**: 113x faster queries with AST caching (452ms → 4ms)
> ✅ **Fuzzy Search**: Jaccard-based candidate filtering (99.8% reduction, 2.2× faster)
> ⚡ **SIMD-Accelerated**: AVX2/SSE4.2/NEON text search (5× E2E normalization, 30× ASCII lowercase)
> ✅ **Full Query Language**: Boolean logic, pattern matching, multi-field queries
> ✅ **100% Local**: No telemetry, no external calls, complete privacy
> ✅ **11,800+ Tests**: Comprehensive test suite across all 35 language plugins, core, CLI, MCP, and LSP
> 🧩 **VSCode Extension (Preview)**: In-editor semantic queries and CodeLens integration

## Top 5 Use Cases

- **Graph Analysis**: `sqry graph trace-path "main" "helper"` - Find execution paths between symbols
- **Dependency Trees**: `sqry graph dependency-tree "module"` - Visualize transitive dependencies
- **Call Chain Depth**: `sqry graph call-chain-depth "function"` - Analyze complexity metrics
- **Cross-Language Detection**: `sqry graph cross-language` - Track TypeScript→JS, Python→C, HTTP boundaries, SQL access, and Dart platform channels
  - Examples:
    - `sqry graph cross-language --from-lang rust --edge-type ffi`
    - `sqry graph cross-language --from-lang sql  --edge-type table`
    - `sqry graph cross-language --edge-type http`
- **Code Visualization**: `sqry visualize "callers:*" --format mermaid` - Generate diagrams

## Output Formats & Preview

- Text with context: `sqry --preview main` (default 3 lines) or `--preview 0` to show only the matched line.
- JSON with context: `sqry --json --preview 5 "pattern"` adds a `context` block with line numbers.
- CSV/TSV export: `sqry --csv --headers --columns name,file,line --preview pattern > results.csv` (use `--tsv` for tabs).
- Safety: CSV/TSV escape RFC 4180 and prefix formula-leading characters unless `--raw-csv` is set.
- Column validation: invalid `--columns` values fail fast with helpful guidance.

## Why sqry?

**ripgrep** is great for text search. **sqry** is great for code search.

### Architecture: Nanosecond-Fast Search

sqry's core competency is **AST/graph-based code search in nanoseconds**:

- **ripgrep/AST/graph** = nanoseconds (SQRY's search engine)
- **100% local**, no external dependencies for search
- Search speed never degraded by slow external services

**What sqry does NOT do**: Put LLMs or embeddings in the search path (that would be seconds, not nanoseconds).

**Future (optional)**: A natural language translation layer may be added to help users/AI agents convert natural language queries into SQRY command syntax. This translation would happen BEFORE search (1-2 seconds for translation, then nanoseconds for search). LLMs would translate, not search.

> **Note**: Prior documentation may have incorrectly described "hybrid search" with embeddings searching code. See [ERRATA.md](ERRATA.md) for the architectural correction. sqry's core remains nanosecond-fast ripgrep/AST/graph search.

### sqry vs Embedding-Based Search: Different Paradigms

**"Semantic search"** has two meanings in code tools:

| Approach | Meaning | sqry? |
|----------|---------|-------|
| **Program Semantics** (PL theory) | What code **IS** - structure, types, behavior | ✅ YES |
| **Distributional Semantics** (ML) | What code **resembles** - vector similarity | ❌ NO |

sqry does **semantic code search** in the PL theory sense: it understands code meaning through AST analysis, not embedding similarity.

#### Why sqry beats embeddings for code understanding

| Task | Embeddings | sqry |
|------|------------|------|
| **"Find all callers of authenticate()"** | Guesses from similar tokens | **Exact list** via `callers:authenticate` |
| **"Find all trait implementations"** | Hopes impl code "looks similar" | **Exact list** via `impl:Iterator` |
| **"Find circular dependencies"** | Cannot detect | **Exact graph** via `circular:calls` |
| **"Find all async functions"** | Top-10 that "look async" | **ALL 357** async functions |

#### Benchmark Results

| Metric | sqry | Embeddings | Notes |
|--------|------|------------|-------|
| **Graph queries** | 12-21ms | 1,300-2,400ms | **100-200x faster** |
| **Results returned** | 257 mean | 10 mean | **25x more complete** |
| **Precision** | 100% | ~40-70% | No false positives |
| **Recall** | 100% | ~60-80% | No missed results |

#### For LLMs and AI Agents (MCP Integration)

sqry provides **ground truth** about code structure that embeddings cannot:

```
# With embeddings: LLM guesses at relationships
LLM: "I found some code that looks related to authentication..."

# With sqry (MCP): LLM knows exact structure
LLM: "There are 23 callers of authenticate():
      - 15 in api/handlers/ (all check return value)
      - 5 in middleware/ (3 don't check return value - BUG?)
      - 3 in tests/"
```

**Unique capabilities no embedding model offers:**
- `callers:X` / `callees:X` - Exact call graph traversal
- `impl:Trait` - All implementations of a trait/interface
- `circular:calls` - Circular dependency detection
- `duplicates:body` - Exact duplicate function detection
- `unused:true` - Dead code analysis

See `sqry-mcp/README.md` for MCP integration details.

## VSCode Extension (Preview)

Bring sqry semantics directly into VSCode:

- Command palette actions: `Sqry: Query…`, `Sqry: Search Workspace…`, `Sqry: Find Semantic References`, `Sqry: Index Workspace`.
- Dedicated "Semantic Results" panel with symbol + text match sections powered by the sqry CLI.
- CodeLens annotations surface caller counts after indexing.
- Settings (`Sqry` namespace) control CLI path, limits, timeouts, auto-index behaviour, and CodeLens visibility.

### Configuration

Configure timeouts in VSCode settings (`.vscode/settings.json` or User Settings):

```json
{
  "sqry.timeoutMs": 15000,        // Timeout for search/query operations (default: 15s)
  "sqry.indexTimeoutMs": 300000,  // Timeout for index rebuilds (default: 5 minutes)
  "sqry.limit": 200,              // Max results per query
  "sqry.autoIndexOnOpen": "prompt" // "always", "prompt", or "never"
}
```

**For large codebases** (10,000+ symbols): If indexing times out, increase `sqry.indexTimeoutMs`:
```json
{
  "sqry.indexTimeoutMs": 600000  // 10 minutes for very large projects
}
```

### Installation

Source code lives in `tools/sqry-vscode/`. Build and run locally:

```bash
cd tools/sqry-vscode
npm install
npm run compile
npm test
```

Launch the extension via VSCode's `F5` Extension Development Host to exercise all features while we finalize Marketplace packaging.

## Language Server Protocol (sqry-lsp)

`sqry lsp` exposes the same semantic engine over the Language Server Protocol so any editor that speaks LSP 3.17 can consume sqry results.

### Quick start

```bash
sqry index .            # build/update the workspace index
sqry lsp --stdio        # start the server (stdio mode)
```

- VSCode (local extension build), Neovim (`lspconfig`), Helix, and other editors simply point their LSP client to `sqry lsp --stdio`. Socket mode (`sqry lsp --socket 127.0.0.1:9257`) lets multiple editors share a single server instance.
- Configuration keys such as `sqry.search.limit`, `sqry.indexRoot`, `sqry.callHierarchy.maxResults`, and `sqry.log.level` can be supplied through LSP `workspace/didChangeConfiguration` or via CLI flags.

### Supported handlers

- Standard LSP: `initialize`, `textDocument/{hover,definition,references,documentSymbol,codeAction}`, `workspace/symbol` (paginated), `textDocument/prepareCallHierarchy`, `callHierarchy/{incomingCalls,outgoingCalls}`, and `workspace/executeCommand`.
- 27 custom sqry methods: search, relations, direct callers/callees, graph stats/export/subgraph, trace-path, semantic diff, duplicates, cycles, unused symbols, complexity metrics, dependency impact, hierarchical search, explain symbol, similar symbols, natural language queries, and more. See `docs/FEATURE_LIST.md` for the full endpoint list and LSP config keys.
- All handlers reuse the persisted `.sqry-index`, so LSP gives identical answers to `sqry query`/`sqry graph` while honouring unsaved changes and cancellation.

### Transport & security

- Default stdio transport keeps the server local to the editor. Socket mode can be combined with TLS (`--tls-cert`/`--tls-key`) and authentication tokens (`--auth-token` or `SQRY_LSP_AUTH_TOKEN`) for remote access.
- `--allow-public-bind`/`SQRY_LSP_ALLOW_PUBLIC_BIND=1` suppresses warnings when binding to non-localhost interfaces; keep it disabled unless you have TLS + auth configured.
- Logs use structured spans (`handler`, `duration_ms`, `outcome`) so `scripts/lsp-perf-analyze.sh` can summarize latency across handlers.

For detailed setup, telemetry guidance, and troubleshooting see the LSP module in `sqry-lsp/`.

### Before (ripgrep)
```bash
# Find all async functions - gets false positives
rg "async fn"
# Also matches: comments, strings, variable names

# Find function definitions - complex regex
rg "fn\s+\w+\s*\("
# Fragile, language-specific, misses edge cases
```

### After (sqry)
```bash
# Find all async functions - semantic understanding
sqry query "kind:function AND async:true"
# Only matches actual function definitions

# Find all public Rust functions
sqry query "kind:function AND lang:rust AND visibility:public"
# Language and type aware with boolean logic

# Find all functions that call a specific function
sqry query "callers:process_data"
# Cross-file semantic analysis - see who calls your function

# Find functions by return type
sqry query "returns:Result"
# Find all functions returning Result<T,E>

# Combine relation queries with metadata
sqry query "kind:function AND visibility:public AND callers:helper"
# Find public functions that call 'helper'

# Instant results with persistent index
sqry index          # Index once
sqry query "..."    # Query instantly
```

## Features

### Graph Architecture
- 📊 **Unified Graph Mode**: 5-10x faster code analysis with graph-based extraction (default)
- ⚡ **Parallel Graph Indexing**: GraphBuilder provides ~3× faster indexing with deterministic, thread-safe output
- 🔍 **Graph Queries**: Path tracing, call-chain depth, dependency trees, cross-language detection, graph stats, cycle detection, complexity metrics
- 🎨 **Multi-Format Visualization**: Export as DOT (Graphviz), Mermaid, D2, or JSON
- 🌐 **Pass 5 Cross-Language Detection**: Multi-pass build pipeline with automatic relationship tracking across language boundaries
  - FFI linking: Rust `extern` declarations matched to C/C++ function definitions
  - HTTP linking: JS/TS `fetch`/`axios` calls matched to Python/Java/Go route handlers
  - SQL table access, Dart MethodChannel invocations, Flutter widget hierarchies
- ⚡ **Performance**: JavaScript (760K LOC/s), C++ (1.1M LOC/s), Python (~500K LOC/s)

### Core Features
- ⚡ **Lightning Fast**: 113x faster queries with multiprocess-safe AST cache (452ms → 4ms)
- 🚀 **Incremental Indexing**: 10-100x faster re-indexing with XXHash3-based change detection
- 🔀 **Git-Aware Updates**: Automatic git change tracking for even faster incremental builds — processes only files changed since last index with graceful fallback to hash-based detection
- 👁️ **Watch Mode**: Real-time index updates with OS-level file monitoring (< 1ms latency)
- 🌐 **35 Languages**: Full symbol extraction across Rust, Python, TypeScript, JavaScript, Go, Java, C/C++, C#, Kotlin, Ruby, Swift, Scala, Lua, R, Dart, PHP, SQL, Shell/Bash, Haskell, Perl, Elixir, Vue, Svelte, Groovy, Zig, HTML, CSS, Terraform, Puppet, Pulumi, Salesforce Apex, SAP ABAP, Oracle PL/SQL, ServiceNow Xanadu
- 🔗 **28 with Full Relation Tracking (Tier 1)**: Extract calls, imports, exports for complete dependency analysis (C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig)
- 📊 **80% Tier 1 Coverage**: 28 of 35 languages with full relation extraction validated end-to-end
- 💾 **Cache Lifecycle**: `sqry cache prune` for time/size-based retention (--days, --size, --dry-run)
- 🎯 **Accurate**: AST-based parsing with boolean query language and security-hardened regex filters
- 🔗 **Cross-file analysis**: Callers, callees, imports, exports, and return-type relations
- 🤖 **Native MCP server**: 33 JSON-RPC tools for AI assistants (Claude, Codex, Gemini, Cursor, Windsurf)
- 🔍 **Fuzzy search**: Jaccard candidate filtering (99.8% reduction on short queries, 2.2× faster)
- 🔌 **Extensible**: Plugin system with clear development guides and test standards
- 💎 **Smart indexing**: Trigram store, string/path interning, persistent cache with Blake3 hashing, `.sqryignore` support (gitignore syntax)
- 🎨 **Rich output**: JSON for tools, colored text for humans, streaming mode for fuzzy CLI
- 🔒 **Private**: 100% local processing — no telemetry, no external calls
- 📊 **Market Position**: #1 in local-first semantic search

## Language Support (35 Languages)

**Tier 1 - Full Relation Support (28 languages)**: Extract calls, imports, exports with CLI validation

**Systems & Low-Level (5)**:
- **C** — [sqry-lang-c](sqry-lang-c/) — Structs, unions, function pointers, full relation tracking ✅
- **C++** — [sqry-lang-cpp](sqry-lang-cpp/) — Templates, classes, namespaces, full relation tracking ✅
- **Rust** — [sqry-lang-rust](sqry-lang-rust/) — Full relation queries (callers, callees, imports, exports, returns) ✅
- **Shell (Bash)** — [sqry-lang-shell](sqry-lang-shell/) — Functions, call tracking, command substitution ✅
- **Zig** — [sqry-lang-zig](sqry-lang-zig/) — Comptime, pub visibility, full relation tracking ✅

**Modern Web (6)**:
- **JavaScript** — [sqry-lang-javascript](sqry-lang-javascript/) — ES6+, async/await, classes, full relation tracking ✅
- **TypeScript** — [sqry-lang-typescript](sqry-lang-typescript/) — Interfaces, generics, JSX, return-type relations ✅
- **Dart** — [sqry-lang-dart](sqry-lang-dart/) — Classes, async/await, Flutter support, full relation tracking ✅
- **Kotlin** — [sqry-lang-kotlin](sqry-lang-kotlin/) — Data classes, coroutines, sealed classes, full relation tracking ✅
- **Swift** — [sqry-lang-swift](sqry-lang-swift/) — Protocols, extensions, async/await, full relation tracking ✅
- **Scala** — [sqry-lang-scala](sqry-lang-scala/) — Case classes, traits, implicits, full relation tracking ✅

**Enterprise (3)**:
- **C#** — [sqry-lang-csharp](sqry-lang-csharp/) — LINQ, async, properties, full relation tracking ✅
- **Go** — [sqry-lang-go](sqry-lang-go/) — Interfaces, channels, goroutines, full relation tracking ✅
- **Java** — [sqry-lang-java](sqry-lang-java/) — Annotations, generics, inheritance, full relation queries ✅

**Scripting & Dynamic (6)**:
- **Python** — [sqry-lang-python](sqry-lang-python/) — Classes, functions, decorators, type hints, full relation tracking ✅
- **Ruby** — [sqry-lang-ruby](sqry-lang-ruby/) — Modules, metaprogramming, blocks, signature metadata ✅
- **PHP** — [sqry-lang-php](sqry-lang-php/) — Traits, namespaces, Laravel/Symfony, full relation tracking ✅
- **Lua** — [sqry-lang-lua](sqry-lang-lua/) — Modules, colon syntax methods, require relations ✅
- **R** — [sqry-lang-r](sqry-lang-r/) — Functions, S3/S4 methods, R6 classes, package metadata ✅
- **Groovy** — [sqry-lang-groovy](sqry-lang-groovy/) — Classes, closures, Gradle tasks, full relation tracking ✅

**Functional & Specialized (4)**:
- **Elixir** — [sqry-lang-elixir](sqry-lang-elixir/) — Phoenix, pipe operators, Erlang FFI, full relation tracking ✅
- **SQL** — [sqry-lang-sql](sqry-lang-sql/) — Tables, views, functions, triggers ✅
- **Svelte** — [sqry-lang-svelte](sqry-lang-svelte/) — Props, reactive declarations, store subscriptions, SFC support ✅
- **Vue** — [sqry-lang-vue](sqry-lang-vue/) — Composition API, options API, SFC support, full relation tracking ✅

**Markup & Styling (2)**:
- **HTML** — [sqry-lang-html](sqry-lang-html/) — Structure extraction, script/link imports ✅
- **CSS** — [sqry-lang-css](sqry-lang-css/) — Selectors, rules, @import tracking ✅

**Additional (2)**:
- **Haskell** — [sqry-lang-haskell](sqry-lang-haskell/) — Module imports, type classes, full relation tracking ✅
- **Perl** — [sqry-lang-perl](sqry-lang-perl/) — Module imports, full relation tracking ✅

**Tier 2 - Domain-Specific & IaC (7 languages)**: Symbol extraction + imports + graph support
- **Terraform (HCL)** — [sqry-lang-terraform](sqry-lang-terraform/) — Resources, modules, variables, outputs
- **Puppet** — [sqry-lang-puppet](sqry-lang-puppet/) — Classes, resources, defined types
- **Pulumi** — [sqry-lang-pulumi](sqry-lang-pulumi/) — Infrastructure resources, stack definitions
- **Salesforce Apex** — [sqry-lang-salesforce-apex](sqry-lang-salesforce-apex/) — Enterprise Apex support
- **SAP ABAP** — [sqry-lang-sap-abap](sqry-lang-sap-abap/) — Enterprise ABAP support
- **Oracle PL/SQL** — [sqry-lang-oracle-plsql](sqry-lang-oracle-plsql/) — Stored procedures
- **ServiceNow Xanadu** — [sqry-lang-servicenow-xanadu](sqry-lang-servicenow-xanadu/) — Script Includes, GlideRecord

**Coverage**: 80% Tier 1 (28/35 languages)

## Quick Start

### Requirements

- **Rust**: 1.90 or later with Edition 2024 (REQUIRED)
- **Git**: For cloning the repository

To install Rust 1.90+:
```bash
# Install rustup if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Update to latest stable (ensure 1.90+)
rustup update stable

# Verify version
rustc --version
# Should show: rustc 1.90.0 or higher
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for contributor guidelines.

### Installation

```bash
# Clone and install (recommended - makes sqry available system-wide)
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo install --path sqry-cli

# Verify installation
sqry --version
```

All 35 language plugins (including IaC: Terraform, Puppet, Pulumi) are included by default.

### Usage

```bash
# Index your codebase (one-time, builds cache for 113x faster queries)
sqry index

# Graph Commands (Unified Graph)
sqry graph stats                         # Show graph statistics
sqry graph trace-path "main" "helper"    # Find shortest path between symbols
sqry graph call-chain-depth "function"   # Calculate max call depth
sqry graph dependency-tree "module"      # Show transitive dependencies
sqry graph cross-language                # List cross-language relationships
sqry graph cross-language --from-lang rust --edge-type ffi             # Rust FFI calls
sqry graph cross-language --from-lang sql  --edge-type table           # SQL table access
sqry graph cross-language --edge-type widget_child                     # Flutter widget hierarchy

# Graph-based queries (instant with index, works across all languages)
sqry query "kind:function AND name~=/test/"
sqry query "kind:class AND lang:rust"
sqry query "kind:function AND async:true"

# Visualize code relationships
sqry visualize "callers:main" --format mermaid
sqry visualize "imports:*" --format graphviz --output-file deps.dot
sqry visualize "callees:process" --format d2 --depth 5

### Graph Visualizations

- Multi-format exports: Graphviz DOT, Mermaid, and D2, plus raw JSON
- Visualize call graphs, dependency trees, cross-language edges, and path traces
- Output to files or stdout; render with your preferred tooling (e.g., Graphviz, Mermaid CLI)

See guides for details and examples:
- Full workflow: [docs/user-guide/visualization.md](docs/user-guide/visualization.md)
- Usage examples: [docs/USAGE_EXAMPLES.md](docs/USAGE_EXAMPLES.md)

# Cache lifecycle management
sqry cache stats                     # View cache statistics and disk usage
sqry cache stats --json              # JSON output for monitoring/automation
sqry cache clear --confirm           # Clear all cached ASTs
sqry cache prune --days 30           # Remove entries older than 30 days
sqry cache prune --size 1GB          # Cap cache to 1GB (oldest first)
sqry cache prune --days 7 --dry-run  # Preview without deleting

# Cache configuration via environment variables
export SQRY_CACHE_ROOT=/mnt/ssd/.sqry-cache    # Custom cache location
export SQRY_CACHE_MAX_BYTES=524288000          # 500 MB limit
export SQRY_CACHE_DISABLE_PERSIST=1            # Disable disk cache

# Cache advanced topics:
# - Per-user cache isolation
# - Multi-repository cache sharing
# - CI/CD integration

# Relation queries (all languages with graph mode)
sqry query "callers:process_data"        # Find who calls this function
sqry query "callees:main"                # Find what this function calls
sqry query "imports:utils"               # Find who imports this module
sqry query "returns:Result"              # Find functions returning Result

# Combine relations with other predicates
sqry query "kind:function AND visibility:public AND callers:helper"

# Keep the session cache warm
sqry shell /path/to/repo                 # Interactive REPL, session stays hot
sqry batch /path/to/repo --queries queries.txt --stats
sqry query --session "kind:function" /path/to/repo  # Warm cache within a single invocation

# (Session tips) `--session` and `sqry shell/batch` require an existing `.sqry-index`.
# For long-lived sessions or scripts that span multiple processes, prefer the shell/batch commands.

# Search without index (slower, on-the-fly parsing)
# Note: Relation queries require an index
sqry search "kind:function" src/

# Update index incrementally
sqry update

# Watch directory for changes (auto-update index)
sqry watch --build          # Start watching with initial index build
sqry watch --stats          # Show statistics for each update
sqry watch --debounce 500   # Custom debounce timing (milliseconds)

# Repair corrupted index
sqry repair --dry-run       # Preview repairs without changes
sqry repair --fix-all       # Fix all detected issues

# Multi-repository workspace management
sqry workspace init /projects --name "My Workspace"
sqry workspace scan /projects
sqry workspace query /projects "kind:function AND repo:backend"
sqry workspace stats /projects

# Exclude files from indexing with .sqryignore
# Create .sqryignore in your project root (same syntax as .gitignore)
echo "node_modules/" >> .sqryignore
echo "target/" >> .sqryignore
echo "*.min.js" >> .sqryignore
# Rebuild index to apply changes
sqry index

# Fuzzy search (typo-tolerant)
sqry --fuzzy "patern" src/          # Finds "pattern", "Pattern", etc.
sqry --fuzzy --fuzzy-stream "fn" .  # Streaming results

# Get help
sqry --help

# List enabled languages (with extensions)
sqry --list-languages

Example excerpt (abbreviated):

```
Enabled languages (...):
- SQL (id: sql, vX.Y.Z): [sql]
- ServiceNow Xanadu (id: servicenow-xanadu-js, vX.Y.Z): [snjs]
```

# Generate shell completions
sqry completions bash > /etc/bash_completion.d/sqry    # Bash
sqry completions zsh > ~/.zfunc/_sqry                 # Zsh
sqry completions fish > ~/.config/fish/completions/sqry.fish  # Fish
sqry completions powershell > sqry.ps1                # PowerShell
```

## Configuration

sqry uses a modern configuration system stored in `.sqry/graph/config/config.json`. Configuration is automatically created on first use with sensible defaults.

### Quick Configuration

```bash
# View effective configuration
cat .sqry/graph/config/config.json

# Common settings to adjust:
# - indexing.max_file_size: Maximum file size to index (default: 10 MB)
# - indexing.max_depth: Directory traversal depth (default: 100)
# - ignore.patterns: Files/directories to exclude
# - include.patterns: Override ignore patterns
# - cache.directory: Cache location (default: .sqry-cache)
```

### Environment Variable Overrides

Control buffer sizes and limits via environment variables:

```bash
# Buffer sizes
export SQRY_READ_BUFFER=16384        # Read buffer (default: 8192)
export SQRY_PARSE_BUFFER=131072      # Parse buffer (default: 65536)

# DoS prevention limits
export SQRY_MAX_SOURCE_FILE_SIZE=104857600  # Max file size (default: 50 MB)
export SQRY_MAX_REPOSITORIES=2000           # Max repos (default: 1000)
export SQRY_MAX_QUERY_LENGTH=20480          # Max query length (default: 10 KB)
```

### Migrating from Legacy Config

If you have an existing `.sqry-config.toml`, sqry automatically migrates it to the new format:

```bash
# Run any sqry command to trigger migration
sqry index .

# Output:
# WARN: Legacy config detected at .sqry-config.toml
#       Migrated to .sqry/graph/config/config.json
#       Consider removing the legacy file after verification
```

Run `sqry config show` to view effective configuration.

## Choosing the Right Command

sqry provides three primary commands for different workflows:

### `sqry search` - Quick On-the-Fly Searches
**When to use**: Exploring unfamiliar codebases, one-off queries, or when you don't want to build an index.

```bash
sqry search "kind:function AND name~=/test/" src/
sqry search "kind:class" --lang rust
```

**Trade-offs**:
- ✅ No index required - works immediately
- ✅ Always reflects current file state
- ❌ Slower (parses files on-demand)
- ❌ No relation queries (callers, callees, etc.)

### `sqry query` - Fast Indexed Queries
**When to use**: Working in a project regularly, need instant results, or using relation queries.

```bash
sqry index          # One-time setup
sqry query "callers:process_data"
sqry query "kind:function AND async:true"
```

**Trade-offs**:
- ✅ 113x faster with cache (452ms → 4ms)
- ✅ Supports relation queries (callers, callees, returns)
- ✅ Incremental updates with `sqry update`
- ❌ Requires initial indexing step
- ❌ Need to update index after changes

### `sqry graph` - Code Structure Analysis
**When to use**: Understanding code architecture, analyzing dependencies, or visualizing relationships.

```bash
sqry graph trace-path "main" "helper"
sqry graph dependency-tree "module"
sqry graph cross-language --from-lang dart --edge-type channel_invoke
```

**Trade-offs**:
- ✅ 5-10x faster than legacy mode
- ✅ Cross-language relationship detection
- ✅ Visualization export (DOT, Mermaid, D2)
- ✅ Cycle detection and path analysis
- ❌ Requires index (same as `sqry query`)

**Quick Reference**:
| Task | Command | Index Required? |
|------|---------|----------------|
| Quick code exploration | `sqry search` | No |
| Find callers/callees | `sqry query` | Yes |
| Trace execution paths | `sqry graph` | Yes |
| Export visualizations | `sqry graph` | Yes |

### Index Flags

- `--no-incremental` — Disable hash-based change detection. Useful for debugging or forcing metadata-only evaluation.
- `--cache-dir <PATH>` — Override cache directory (default: `.sqry-cache`). Helpful for ephemeral or sandboxed environments.
- `--no-compress` — Accepted for forward compatibility but not currently wired into the unified graph build pipeline. The snapshot format uses postcard serialization with length-prefixed framing.
- `--validate [off|warn|fail]` — Index validation strictness (default: `warn`). Controls how to handle index corruption:
  - `off`: Skip validation entirely (fastest)
  - `warn`: Log warnings but continue (default)
  - `fail`: Abort on validation errors
- `--auto-rebuild` — Automatically rebuild index if validation fails (requires `--validate`). When set with `--validate=fail`, sqry will rebuild the index once and retry on corruption.

Examples:

```bash
# Force full rebuild without incremental detection
sqry index . --no-incremental

# Use a custom cache directory for hash index storage
sqry index . --cache-dir /tmp/sqry-cache

# Update with custom cache path and incremental disabled
sqry update . --no-incremental --cache-dir /tmp/sqry-cache

# Validate index with strict failure mode
sqry index --validate=fail

# Check index status with validation report
sqry index --status --validate=warn

# Export validation metrics (Prometheus or JSON)
# Prometheus/OpenMetrics text:
sqry index --status --json -M prom > metrics.txt
# Force JSON explicitly (default):
sqry index --status --json -M jsn > status.json

# Update index with auto-rebuild on corruption
sqry update --validate=fail --auto-rebuild
```

**Exit Codes** (with validation):
- `0`: Success
- `1`: Runtime error (file not found, permission denied, etc.)
- `2`: Validation error (corruption detected with `--validate=fail`)

See `sqry index --help` for all validation options.

### Environment Variables

sqry supports optional environment variables for performance tuning:

#### Query Lexer Pool

The query lexer uses thread-local buffer pooling to reduce allocations. This is automatically enabled by default and requires no configuration for most use cases.

```bash
# Pool size per thread (default: 4)
export SQRY_LEXER_POOL_MAX=8

# Buffer capacity limit in tokens (default: 256)
export SQRY_LEXER_POOL_MAX_CAP=512

# Shrink ratio when buffer exceeds capacity (default: 8)
export SQRY_LEXER_POOL_SHRINK_RATIO=4

# Disable pooling for latency-critical workloads (default: enabled)
export SQRY_LEXER_POOL_MAX=0
```

**When to adjust**:
- Keep defaults for most workloads (recommended)
- Set `SQRY_LEXER_POOL_MAX=0` only for micro-benchmarking or <1ms latency requirements
- Increase `SQRY_LEXER_POOL_MAX` for high-concurrency server workloads

See [docs/PERFORMANCE_TUNING.md](docs/PERFORMANCE_TUNING.md) for detailed performance characteristics.

#### Cache Configuration

```bash
# Cache root directory (default: .sqry-cache)
export SQRY_CACHE_ROOT=/mnt/fast-ssd/.sqry-cache

# Maximum cache size in bytes (default: 52428800 = 50 MB)
export SQRY_CACHE_MAX_BYTES=524288000  # 500 MB

# Disable persistent cache (default: enabled)
export SQRY_CACHE_DISABLE_PERSIST=1

# Select eviction policy (lru, tiny_lfu, hybrid)
export SQRY_CACHE_POLICY=tiny_lfu

# Protected window ratio for TinyLFU (0.05-0.95, default 0.20)
export SQRY_CACHE_POLICY_WINDOW=0.25

# Always emit CacheStats{...} on stderr (even without --debug-cache)
export SQRY_CACHE_DEBUG=1
```

**When to use**:
- Set `SQRY_CACHE_ROOT` for custom cache locations (SSDs, shared storage)
- Set `SQRY_CACHE_MAX_BYTES` to cap disk usage
- Set `SQRY_CACHE_DISABLE_PERSIST=1` for memory-only caching in containers
- Use `SQRY_CACHE_POLICY` / `SQRY_CACHE_POLICY_WINDOW` to experiment with TinyLFU before switching defaults (telemetry proves ≥20% warm-hit wins).
- Use `SQRY_CACHE_DEBUG=1` in CI to capture `CacheStats{...}` lines without modifying CLI invocations.

**Per-user isolation**: Cache entries are isolated per user by default (based on `$USER`).

#### Hybrid Search Configuration

```bash
# Control search fallback behavior
export SQRY_FALLBACK_ENABLED=true           # Enable text search fallback (default: true)
export SQRY_MIN_SEMANTIC_RESULTS=10         # Min results before fallback (default: 1)
export SQRY_MAX_TEXT_RESULTS=1000           # Max text search results (default: 1000)
export SQRY_TEXT_CONTEXT_LINES=2            # Context lines for text search (default: 2)
export SQRY_SHOW_SEARCH_MODE=true           # Show which mode was used (default: true)
```

#### Session Management

```bash
# Configure session caching behavior
export SQRY_SESSION_CACHE_SIZE=100          # Session cache size (default: auto)
export SQRY_SESSION_TIMEOUT=1800            # Session timeout in seconds (default: 1800)
export SQRY_SESSION_CLEANUP_INTERVAL=300    # Cleanup interval in seconds (default: 300)
export SQRY_NO_SESSION=1                    # Disable session caching (default: false)
```

#### Performance Tuning (Advanced)

```bash
# Buffer sizes (auto-detected by default)
export SQRY_INDEX_BUFFER=8192               # Index write buffer size
export SQRY_PARSE_BUFFER=8192               # Parse buffer size
export SQRY_READ_BUFFER=8192                # File read buffer size
export SQRY_WRITE_BUFFER=8192               # File write buffer size
export SQRY_MAX_INDEX_SIZE=524288000        # Maximum index size (default: 500 MB)
```

#### Git Integration

```bash
# Configure Git backend behavior
export SQRY_GIT_BACKEND=libgit2             # Git backend (libgit2 or git2)
export SQRY_GIT_INCLUDE_UNTRACKED=false     # Include untracked files (default: false)
export SQRY_GIT_RENAME_SIMILARITY=50        # Rename similarity 0-100 (default: 50)
export SQRY_GIT_TIMEOUT_MS=5000             # Git operation timeout (default: 5000)
```

#### Watch Mode

```bash
# Configure file watching debounce timing
export SQRY_LIMITS__WATCH__DEBOUNCE_MS=100  # Debounce in milliseconds (default: 100-400 platform-dependent)
```

See [docs/PERFORMANCE_TUNING.md](docs/PERFORMANCE_TUNING.md) for performance tuning details.

### Fuzzy Search

sqry supports fuzzy symbol search with **Jaccard similarity** for intelligent candidate filtering:

```bash
# Enable fuzzy matching (requires index)
sqry --fuzzy "patern" .              # Finds "pattern", "Pattern", etc.

# Configure fuzzy algorithm (jaro-winkler or levenshtein)
sqry --fuzzy --fuzzy-algorithm levenshtein "execute" .

# Adjust similarity threshold (0.0-1.0, default: 0.6)
sqry --fuzzy --fuzzy-threshold 0.8 "strict" .

# Enable streaming mode for incremental results
sqry --fuzzy --fuzzy-stream "search" .

# Debug mode shows scoring and candidate filtering metrics
env RUST_LOG=debug sqry --fuzzy --fuzzy-stream "pattern" .
```

**Features:**
- ⚡ **Jaccard similarity** for candidate filtering - reduces candidates by up to 99.9% for short queries
- 🎯 Configurable algorithms (Jaro-Winkler, Levenshtein)
- 📊 Parallel scoring with optional streaming
- 🔧 Tunable thresholds and parameters
- 🔙 Backward compatible with old indices

**Configuration**:
- Jaccard filtering enabled by default for optimal performance
- Disable via `SQRY_FUZZY_USE_JACCARD=0` if needed (uses legacy ratio method)
- Jaccard filtering enabled by default for optimal performance

**Note**: Fuzzy search requires a pre-built index (`sqry index`).

### Hybrid Search Modes

Control search strategy for optimal results:

```bash
# Force text-only search (faster for simple patterns)
sqry search "TODO" --text

# Force semantic search (AST-aware only)
sqry search "main" --semantic

# Disable automatic fallback to text search
sqry search "pattern" --no-fallback

# Configure text search context lines
sqry search "error" --text --context 5

# Limit text search results
sqry search "test" --text --max-text-results 500
```

**How hybrid search works** (semantic + text fallback):
1. First tries semantic (AST-based) search
2. If < threshold results found, falls back to text search (ripgrep)
3. Use `--text` for simple patterns, `--semantic` for precise symbol queries
4. Use `--no-fallback` to enforce strict mode

> **Note**: "Hybrid search" here refers to combining semantic (AST) and text (ripgrep) search, NOT embedding-based search. All search happens in nanoseconds.

**Environment variables**: Configure fallback behavior via `SQRY_FALLBACK_ENABLED`, `SQRY_MIN_SEMANTIC_RESULTS`, and `SQRY_MAX_TEXT_RESULTS` (see Environment Variables section).

### Query Performance Options

Optimize query execution and debugging:

```bash
# Explain query execution plan (debugging)
sqry query "kind:function" --explain

# Use persistent session (faster repeated queries)
sqry query "kind:function" --session

# Verbose output with cache statistics
sqry query "kind:function" --verbose

# Emit TinyLFU/LRU telemetry for one-off investigations
sqry query "kind:function" --debug-cache

# Force CacheStats output in CI without changing CLI flags
SQRY_CACHE_DEBUG=1 sqry query "kind:function" .

# Interactive shell (keeps index hot)
sqry shell /path/to/repo

# Batch processing (shared cache)
sqry batch . --queries queries.txt --stats
```

**Performance comparison**:
- Without session: ~200ms per query (cold index load)
- With `--session`: ~50ms per query (hot index)
- Interactive `shell`: <10ms per query (persistent session)

**When to use**:
- `--session`: Multiple queries in quick succession
- `sqry shell`: Interactive exploration
- `sqry batch`: Running multiple queries from file
- `--debug-cache` / `SQRY_CACHE_DEBUG=1`: Capture `CacheStats{...}` (policy, hits, misses, `lfu_rejects`, etc.) on stderr to validate cache behaviour or benchmark TinyLFU. Enabling this flag temporarily forces semantic-only execution so telemetry reflects the AST cache.

### Alternative: Build Without Installing

```bash
# Build only (binary at target/release/sqry)
cargo build --release --package sqry-cli
./target/release/sqry main src/
```

See [sqry-cli/README.md](sqry-cli/README.md) for complete CLI documentation.

## MCP Integrations

Use sqry directly from your IDE via the Model Context Protocol (MCP).

See [sqry-mcp/README.md](sqry-mcp/README.md) for assistant-specific setup (Codex, Gemini, Claude Desktop, Windsurf, Cursor).
See [docs/LLM_SKILLS_STANDARD.md](docs/LLM_SKILLS_STANDARD.md) for shared context/skill definitions across Codex, Claude, and Gemini.

Native MCP server (production):
- ✅ 33 tools across search, relations, graph analysis, navigation, and diff (see `sqry-mcp/README.md`)
- ✅ Layer 2 on-demand documentation resources (tool-guide, query-syntax, patterns, architecture)
- ✅ JSON-RPC 2.0 over stdio, deadlines/timeouts, pagination, structured errors
- ✅ Multi-workspace cache isolation with GraphIdentity-based cache keys
- ✅ Tracing and path-safety guards; comprehensive integration tests

See `sqry-mcp/README.md` for setup, tool schemas, and Codex/Claude Desktop configuration examples, including environment variables.

Legacy: a bash-based MCP server remains available for portability.


## Project Structure

This is a Rust workspace with 55 crates:

- **sqry-core**: Core library — graph engine, symbols, search, query parser, plugin system, cache
- **sqry-cli**: Command-line interface (`sqry` binary) — index, query, graph, visualize, and 30+ commands
- **sqry-lsp**: Language Server Protocol server — hover, definition, references, call hierarchy, 27 custom methods
- **sqry-mcp**: MCP server — 33 JSON-RPC tools for AI assistants (Claude, Codex, Gemini, Cursor, Windsurf)
- **sqry-nl**: Natural language → sqry query translation (optional ONNX classifier)
- **sqry-plugin-registry**: Single source of truth for all 35 built-in language plugins
- **sqry-lang-support**: Shared helpers for language plugins (relation extraction utilities)
- **sqry-lang-\***: 35 language plugins (one crate per language)
- **sqry-mcp-redaction**: Client-side MCP response redaction library
- **sqry-tree-sitter-support**: Tree-sitter binding helpers
- **sqry-test-support**: Test infrastructure (verbose logging, artifacts)
- **sqry-test-fixtures**: Shared test fixture data
- **crates/tree-sitter-\*-sqry**: Vendored tree-sitter grammars (Vue, Svelte, Groovy, ABAP, PL/SQL)

## Development Roadmap

See [CHANGELOG.md](CHANGELOG.md) for release history.

Current development focuses on graph architecture enhancements, cross-language detection, multi-workspace support, and supply chain security.


## Philosophy

sqry is laser-focused on doing **one thing exceptionally well**: semantic code search.

We explicitly **do not** aim to be:
- ❌ A linter (ESLint/Clippy)
- ❌ A monitoring platform (Prometheus)
- ❌ An IDE (VS Code)
- ❌ A server (Sourcegraph)
- ❌ A language-specific analyzer

See [docs/FEATURE_LIST.md](docs/FEATURE_LIST.md) for complete feature documentation.

## Contributing

We welcome contributions! See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

### Development Process


#### Quick Start: UUID-Based Review Workflow

Our development process uses three AI agents (Codex, Gemini, and Claude Code) for comprehensive code review:

**Prerequisites:**
```bash
# Install uuid CLI tool (one-time setup)
cargo install uuid-cli

# Verify installation
uuid --version
```

**Gemini CLI Configuration:**


```bash
# Backup existing settings
cp ~/.gemini/settings.json ~/.gemini/settings.json.backup.$(date +%Y%m%d_%H%M%S)

# Ensure WriteFileTool is enabled in ~/.gemini/settings.json
```

**Request Reviews:**
```bash
# Pre-implementation review (Codex - Technical Arbiter)
  --agent codex \

# Pre-implementation review (Gemini - Alternative Perspective)
  --agent gemini \

# Pre-implementation review (Claude Code - Implementation Validation)
  --agent claude \
```

**Key Benefits:**
- ✅ **Complete audit trail** - Every review iteration preserved with unique UUID
- ✅ **No overwrites** - Previous reviews never lost
- ✅ **Time-sortable** - UUIDv7 includes timestamp for chronological ordering
- ✅ **Automatic iteration tracking** - Script detects iter1, iter2, iter3 automatically
- ✅ **Multi-agent validation** - Three independent perspectives on every change

**File Structure Example:**
```
├── <name>_PLAN.md                                                          # Template (no UUID)
├── <name>_review_request_pre_codex.md                                      # Template (no UUID)
├── <name>_review_request_pre_codex_iter1_019ab405-5a52-7000-b550-91d2ba0045ce.md    # iter1 (UUID)
├── <name>_review_pre_codex_iter1_019ab405-5a52-7000-b550-91d2ba0045ce.md             # iter1 review (UUID)
├── <name>_review_request_pre_codex_iter2_019ab41e-9d2a-7000-b9c5-f09209a8e5ad.md    # iter2 (UUID)
└── <name>_review_pre_codex_iter2_019ab41e-9d2a-7000-b9c5-f09209a8e5ad.md             # iter2 review (UUID)
```

For complete documentation on the development process, see:

## Documentation

### Getting Started
- **[Quick Start Guide](QUICKSTART.md)** - 5-minute tutorial for new users
- **[Usage Examples](docs/USAGE_EXAMPLES.md)** - Real-world usage examples
- **[Feature List](docs/FEATURE_LIST.md)** - Complete feature documentation

### Reference
- [CLI Documentation](sqry-cli/README.md) - Command-line interface guide
- [MCP Server](sqry-mcp/README.md) - MCP tools and AI assistant integration
- [LLM Skills Standard](docs/LLM_SKILLS_STANDARD.md) - Shared context/skill definitions for Codex, Claude, and Gemini
- [Schema Reference](docs/SCHEMA.md) - Metadata keys and query short forms
- [Performance Tuning](docs/PERFORMANCE_TUNING.md) - Optimization guide

#### Tree-Sitter Binding Regeneration (Vue / Svelte / Groovy)

1. Install the CLI once:
   ```bash
   cargo install tree-sitter-cli
   ```
2. Update a grammar by running:
   ```bash
   scripts/update-tree-sitter-bindings.sh <vue|svelte|groovy> <upstream-commit>
   ```
   - Script checks out the vendored submodule, regenerates `parser.c`/`scanner.c`, and overwrites the Rust bindings.
3. Verify the crates build and tests pass:
   ```bash
   cargo build --workspace
   cargo test -p tree-sitter-vue-sqry -p tree-sitter-svelte-sqry -p tree-sitter-groovy-sqry
   ```
4. Commit both the vendor changes and crate updates in the same PR.

> **Note**: Vue and Svelte plugins provide full SFC (Single File Component) support with Component nodes and Contains edges.

Fixtures that exercise relation edges live under:

### Performance
- [Performance Tuning](docs/PERFORMANCE_TUNING.md) - Optimization and benchmarking guide

## Security & Supply Chain Guarantees


## Comparison to Alternatives

| Feature | sqry | ripgrep | ast-grep | Sourcegraph | Semgrep |
|---------|------|---------|----------|-------------|---------|
| Text search | ✅ | ✅ | ❌ | ✅ | ✅ |
| AST awareness | ✅ | ❌ | ✅ | ✅ | ✅ |
| Symbol navigation | ✅ | ❌ | ✅ | ✅ | ✅ |
| Relation queries | ✅ (28 langs) | ❌ | ❌ | ✅ | ✅ |
| Languages | 35 | All text | 15 | 40+ | 30+ |
| Speed | ⚡⚡⚡⚡ | ⚡⚡⚡⚡⚡ | ⚡⚡⚡⚡ | ⚡⚡⚡ | ⚡⚡ |
| Local/offline | ✅ | ✅ | ✅ | ❌ | ✅ |
| Cost | Free | Free | Free | $49/user/mo | $27/dev/mo |
| Plugin system | ✅ | ❌ | ❌ | ❌ | ✅ |
| MCP integration | ✅ (33 tools) | ❌ | ❌ | ❌ | ❌ |
| LSP support | ✅ | ❌ | ❌ | ✅ | ❌ |

**Market Position**: sqry is #1 in local-first semantic code search.

## License

MIT - see [LICENSE-MIT](LICENSE-MIT)

## Acknowledgments

sqry builds on lessons learned from previous code search tools, focusing on core semantic search capabilities.

Special thanks to:
- **tree-sitter** - Fast, incremental parsing library
- **ripgrep** - World-class text search engine (integrated as library)
- The open-source community for language grammar support

---

**Developed by Verivus Labs**
Licensed under MIT - Free and open source forever
