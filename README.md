# sqry - by Verivus

sqry is a semantic code search tool. It parses source code into an AST and builds a graph of symbols and relationships, so you can search by what code **means** rather than just what it says.

Website: https://sqry.dev

## Example

```bash
# Index a codebase (creates .sqry/graph/snapshot.sqry)
sqry index

# Find all public async functions
sqry query "kind:function AND async:true AND visibility:public"

# Who calls this function?
sqry query "callers:authenticate"

# Trace a path between two symbols
sqry graph trace-path main handle_request

# Find circular dependencies
sqry cycles

# Ask in plain English (translates to sqry syntax, then searches)
sqry ask "find all error handling functions"
```

## Why sqry?

If you just need text search, use [ripgrep](https://github.com/BurntSushi/ripgrep) - it's faster and simpler for that job.

sqry is useful when you need to search by code **structure**: finding all callers of a function, tracing execution paths, detecting cycles, or querying by symbol kind, visibility, or return type. These are things text search can't do reliably.

### What sqry is good at

- **Relation queries**: `callers:X`, `callees:X`, `imports:X`, `returns:Result` - exact answers from the call graph
- **Graph analysis**: trace paths between symbols, find cycles, detect unused code
- **Structured queries**: combine predicates with boolean logic (`kind:function AND lang:rust AND async:true`)
- **Cross-language detection**: FFI linking (Rust<>C/C++), HTTP route matching (JS/TS<>Python/Java/Go)
- **AI assistant integration**: MCP server with 33 tools, LSP server for editors

### When NOT to use sqry

- **Simple text search**: Use ripgrep. It's faster for grep-style patterns.
- **Pattern matching on syntax**: Use [ast-grep](https://github.com/ast-grep/ast-grep). It's better for "find this syntax pattern and replace it."
- **Linting or rule enforcement**: Use language-specific linters (clippy, eslint, semgrep).
- **Full IDE features**: Use your editor's built-in language server.
- **Hosted code search**: Use Sourcegraph if you need a web UI and team features.

sqry is focused on one thing: local semantic code search via AST analysis.

## Install

### Option 1: Binary installer (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
```

Installs `sqry`, `sqry-mcp`, and `sqry-lsp` into `~/.local/bin` (override with `--install-dir`).
Add `--verify-signatures` to enforce Cosign bundle verification (requires `cosign`).

### Option 2: Build from source

```bash
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo install --path sqry-cli
cargo install --path sqry-mcp
cargo install --path sqry-lsp

sqry --version
```

**Requirements**: Rust 1.90+ (Edition 2024). About 20 GB disk for a full build (35 tree-sitter grammars are compiled from source).

### Package manager configs

Release packaging configs are generated from signed release checksums for:
- Homebrew (tap formula)
- Scoop
- Winget
- AUR
- Nix
- Snap

Look for these files in release assets (`homebrew-sqry.rb`, `scoop-sqry.json`, `winget-*.yaml`, `aur-PKGBUILD`, `nix-*.nix`, `snap-snapcraft.yaml`).

## Commands

### Search and Query

```bash
# On-the-fly search (no index needed, slower)
sqry search "kind:function" src/

# Indexed query (fast, requires sqry index first)
sqry query "kind:class OR kind:struct"
sqry query "kind:function AND visibility:public AND callers:helper"

# Fuzzy search (typo-tolerant)
sqry --fuzzy "patern" .

# Hierarchical search (results grouped by file, useful for RAG)
sqry hier "kind:function visibility:public"
```

### Relations

```bash
sqry query "callers:process_data"       # Who calls this?
sqry query "callees:main"               # What does this call?
sqry query "imports:utils"              # Who imports this?
sqry query "returns:Result"             # Functions returning Result

sqry graph direct-callers parse_config  # Direct callers
sqry graph direct-callees main          # Direct callees
sqry graph call-hierarchy parse_config --depth 3
```

### Graph Analysis

```bash
sqry graph trace-path main handle_error    # Execution path between symbols
sqry graph dependency-tree module           # Transitive dependencies
sqry graph cross-language                   # Cross-language relationships
sqry cycles                                 # Circular dependencies
sqry duplicates                             # Duplicate code
sqry unused                                 # Dead code detection
sqry impact parse_config                    # Dependency impact analysis
sqry graph complexity                       # Complexity metrics
sqry diff HEAD~1 HEAD                       # Semantic diff between git refs
```

### Natural Language

```bash
sqry ask "What functions handle error recovery?"
sqry ask "Find all public structs" --auto-execute
```

### Export and Visualization

```bash
sqry export --format dot          # Graphviz DOT
sqry export --format mermaid      # Mermaid
sqry export --format d2           # D2 diagrams
sqry export --format json         # JSON

sqry visualize "callers:main" --format mermaid
```

See [docs/user-guide/visualization.md](docs/user-guide/visualization.md) for examples.

### Output Formats

```bash
sqry --preview main                     # Text with context (default 3 lines)
sqry --json --preview 5 "pattern"       # JSON with context
sqry --csv --headers --columns name,file,line pattern > results.csv
```

CSV/TSV output escapes per RFC 4180 and prefixes formula-leading characters unless `--raw-csv` is set.

### Cache Management

```bash
sqry cache stats                    # View cache statistics
sqry cache prune --days 30          # Remove old entries
sqry cache prune --size 1GB         # Cap cache size
sqry cache clear --confirm          # Clear all cached ASTs
```

### Other

```bash
sqry shell                          # Interactive REPL
sqry batch commands.txt             # Batch processing
sqry watch --build                  # Watch mode (auto-update index)
sqry repair --dry-run               # Check for index corruption
sqry --list-languages               # List supported languages
sqry completions bash               # Shell completions
```

## Choosing the Right Command

| Task | Command | Needs Index? |
|------|---------|:---:|
| Quick exploration | `sqry search` | No |
| Find callers/callees | `sqry query` | Yes |
| Trace execution paths | `sqry graph trace-path` | Yes |
| Compare semantic changes across refs | `sqry diff` | Yes |
| Find cycles | `sqry cycles` | Yes |
| Export diagrams | `sqry export` | Yes |

`sqry search` parses files on demand - useful for one-off queries. `sqry query` and `sqry graph` use a prebuilt index and are much faster for repeated use.

## Language Support

sqry supports **35 languages** through tree-sitter-based plugins. 28 of these have full relation extraction (calls, imports, exports); the remaining 7 have symbol extraction with basic import tracking.

**Full relation support (28)**: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig

**Symbol extraction + imports (7)**: Terraform, Puppet, Pulumi, Salesforce Apex, SAP ABAP, Oracle PL/SQL, ServiceNow Xanadu

Each language plugin lives in its own crate (`sqry-lang-*`) and implements the `GraphBuilder` trait. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to add a new language.

## MCP Server (AI Assistant Integration)

sqry includes an MCP server with 33 JSON-RPC tools for AI assistants (Claude, Codex, Gemini, Cursor, Windsurf):

```bash
# Start MCP server (stdio transport)
sqry-mcp

# Auto-configure for your AI assistant
sqry mcp setup

# Or add to .claude/settings.json manually:
# "mcpServers": { "sqry": { "command": "sqry-mcp", "args": [] } }
```

The MCP server gives AI assistants structured access to the code graph - exact caller/callee lists, path tracing, cycle detection, etc. - rather than relying on text search or embedding similarity.

See [sqry-mcp/README.md](sqry-mcp/README.md) for setup details and tool documentation.
For sensitive repositories, use [sqry-mcp-redaction](sqry-mcp-redaction/README.md) to sanitize MCP responses before sending data to external LLMs.

## LSP Server (Editor Integration)

```bash
sqry lsp --stdio        # Start LSP server (stdio mode)
```

Supports hover, definition, references, call hierarchy, document/workspace symbols, code actions, and 27 custom sqry methods. Works with VS Code, Neovim, Helix, and any LSP 3.17 client. Socket mode available for shared instances.

See `sqry-lsp/` for configuration details.

## VSCode Extension (Preview)

The VS Code extension provides in-editor semantic queries, a "Semantic Results" panel, and CodeLens caller count annotations. Source is in `tools/sqry-vscode/`.

```bash
cd tools/sqry-vscode && npm install && npm run compile
# Launch via F5 in VS Code
```

## Configuration

sqry stores configuration in `.sqry/graph/config/config.json`, created automatically with sensible defaults on first use.

```bash
# View configuration
sqry config show

# Common environment variable overrides
export SQRY_CACHE_ROOT=/mnt/ssd/.sqry-cache    # Custom cache location
export SQRY_CACHE_MAX_BYTES=524288000           # 500 MB cache limit
export SQRY_CACHE_DISABLE_PERSIST=1             # Memory-only caching
```

See [docs/PERFORMANCE_TUNING.md](docs/PERFORMANCE_TUNING.md) for all tuning options and [CONFIGURATION_TUNING_GUIDE.md](CONFIGURATION_TUNING_GUIDE.md) for the full reference.

### Index Flags

```bash
sqry index . --no-incremental       # Force full rebuild
sqry index --validate=fail          # Strict validation (exit 2 on corruption)
sqry update --validate=fail --auto-rebuild  # Auto-rebuild on corruption
```

### .sqryignore

Exclude files from indexing using gitignore syntax:

```bash
echo "node_modules/" >> .sqryignore
echo "target/" >> .sqryignore
sqry index
```

## How It Works

sqry builds a code graph in 5 passes:

1. **AST parsing** - tree-sitter parses source files into syntax trees
2. **Symbol extraction** - nodes (functions, classes, etc.) and structural edges (defines, contains)
3. **Enrichment** - visibility, types, async markers
4. **Intra-file edges** - calls, references within each file
5. **Cross-file edges** - import resolution, FFI linking, HTTP route matching

The resulting graph is persisted to `.sqry/graph/snapshot.sqry` and loaded for queries. Graph queries use precomputed analyses (SCCs, condensation DAGs, 2-hop labels) for fast cycle detection, path finding, and reachability.

### Architecture

sqry uses arena-based storage with CSR (Compressed Sparse Row) adjacency for cache-friendly O(1) traversal, generational indices for safety, MVCC for concurrent reads, and string interning to reduce memory. Parallelism uses [rayon](https://github.com/rayon-rs/rayon); locks use [parking_lot](https://github.com/Amanieu/parking-lot).

### Performance Notes

These numbers are from sqry's own codebase (~384K nodes, ~1.3M edges). Your results will vary depending on codebase size, language mix, and hardware.

- **Indexed queries**: ~4ms with warm cache (vs ~452ms cold)
- **Cycle check**: <1s (precomputed SCC lookup)
- **Path finding**: <1s typical (condensation DAG pruning)
- **Indexing throughput**: varies by language (JavaScript ~760K LOC/s, C++ ~1.1M LOC/s, Python ~500K LOC/s on the test machine)

## Project Structure

```
sqry/
├── sqry-core/              # Core library (graph, symbols, search, plugin system)
├── sqry-cli/               # CLI binary
├── sqry-lsp/               # LSP server
├── sqry-mcp/               # MCP server (33 tools for AI assistants)
├── sqry-nl/                # Natural language query translation
├── sqry-plugin-registry/   # Plugin registration
├── sqry-lang-*/            # 35 language plugins
├── sqry-lang-support/      # Plugin infrastructure
├── sqry-tree-sitter-support/ # Tree-sitter helpers
├── sqry-test-support/      # Test infrastructure
└── crates/tree-sitter-*    # Vendored tree-sitter grammars
```

## Comparison

| Feature | sqry | ripgrep | ast-grep | Sourcegraph |
|---------|------|---------|----------|-------------|
| Text search | Yes (with fallback) | Yes | No | Yes |
| AST awareness | Yes | No | Yes | Yes |
| Relation queries | Yes (28 langs) | No | No | Yes |
| Local/offline | Yes | Yes | Yes | No |
| MCP server | Yes (33 tools) | No | No | No |
| LSP server | Yes | No | No | Yes |
| Cost | Free (MIT) | Free | Free | Paid |

This is a rough comparison; each tool has different strengths. ripgrep is faster than sqry for plain text search. ast-grep is better for syntax pattern matching. Sourcegraph offers team features sqry doesn't. sqry's strength is local graph-based queries (callers, cycles, paths).

## Contributing

We welcome contributions of all sizes - from typo fixes to new language plugins. See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
cargo build --workspace                              # Build
cargo test --workspace                               # Tests
cargo fmt --all                                      # Format
cargo clippy --all-targets --workspace -- -D warnings  # Lint
```

## Documentation

- [Quick Start Guide](QUICKSTART.md) - Get started in 5 minutes
- [Usage Examples](docs/USAGE_EXAMPLES.md) - Real-world examples
- [Feature List](docs/FEATURE_LIST.md) - Complete feature reference
- [Schema Reference](docs/SCHEMA.md) - Query syntax and metadata keys
- [Performance Tuning](docs/PERFORMANCE_TUNING.md) - Optimization guide
- [CLI Documentation](sqry-cli/README.md) - Full CLI reference
- [MCP Server](sqry-mcp/README.md) - AI assistant integration
- [Troubleshooting](docs/TROUBLESHOOTING.md) - Common issues and solutions

## Acknowledgments

sqry depends on excellent open-source projects:

- [tree-sitter](https://github.com/tree-sitter/tree-sitter) - Incremental parsing (and the many grammar maintainers)
- [ripgrep](https://github.com/BurntSushi/ripgrep) - Text search engine (used as a library for fallback search)
- [rayon](https://github.com/rayon-rs/rayon) - Data parallelism
- [serde](https://github.com/serde-rs/serde) - Serialization
- [clap](https://github.com/clap-rs/clap) - CLI argument parsing
- [tokio](https://github.com/tokio-rs/tokio) - Async runtime (LSP/MCP servers)

## License

MIT - see [LICENSE-MIT](LICENSE-MIT)

---

Developed by [Verivus](https://verivus.com)
