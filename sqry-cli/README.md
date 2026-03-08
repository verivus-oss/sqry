# sqry CLI - by Verivus

> Semantic code search tool that understands code structure through AST analysis.

## Overview

`sqry` is a command-line tool for searching code by **what it means**, not just what it says. It parses source code into an AST using tree-sitter, builds a graph of symbols and relationships, and lets you query that graph with structured predicates.

**35 languages** supported. See [QUICKSTART.md](../QUICKSTART.md) for the full list.

## Installation

```bash
# Clone and install
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo install --path sqry-cli

# Verify
sqry --version
```

Requires **Rust 1.90+** (Edition 2024). The full build (including 35 tree-sitter grammars) needs ~20 GB disk space.

## Quick Start

```bash
# Build the index (one-time per project)
sqry index .

# Search for symbols
sqry main

# Structured query with predicates
sqry query "kind:function AND name~=/handle/"

# Natural language query
sqry ask "find all async functions in Rust"

# Find callers of a function
sqry graph direct-callers process_request

# Find circular dependencies
sqry cycles --type imports
```

## Commands

### Search & Query

| Command | Description |
|---------|-------------|
| `sqry <pattern>` | Pattern search (regex by default, `--exact` for literal, `--fuzzy` for fuzzy) |
| `sqry search <pattern>` | Explicit search command (same as above) |
| `sqry query <query>` | Structured AST-aware query with predicates |
| `sqry ask <question>` | Natural language query (translates to sqry query syntax) |
| `sqry hier <query>` | Hierarchical search with file/container grouping (RAG-optimized) |

### Index Management

| Command | Description |
|---------|-------------|
| `sqry index [path]` | Build symbol index (stored at `.sqry/graph/snapshot.sqry`) |
| `sqry update [path]` | Incremental index update (changed files only) |
| `sqry watch [path]` | File watcher with auto-update |
| `sqry analyze [path]` | Build precomputed graph analyses (SCCs, condensation) |
| `sqry repair [path]` | Repair corrupted index |

### Graph Analysis

| Command | Description |
|---------|-------------|
| `sqry graph direct-callers <symbol>` | Find direct callers |
| `sqry graph direct-callees <symbol>` | Find direct callees |
| `sqry graph trace-path <from> <to>` | Find call paths between symbols |
| `sqry graph deps <symbol>` | Transitive dependency tree |
| `sqry graph cross-language` | List cross-language relationships |
| `sqry graph stats` | Graph statistics |
| `sqry graph complexity` | Complexity metrics |
| `sqry graph cycles` | Cycle detection (alias for `sqry cycles`) |

### Standalone Analysis

| Command | Description |
|---------|-------------|
| `sqry duplicates` | Find duplicate functions/signatures/structs |
| `sqry cycles` | Detect circular dependencies (calls, imports, modules) |
| `sqry unused` | Find unreachable or unused symbols |
| `sqry impact <symbol>` | Reverse dependency analysis |
| `sqry diff <base> <target>` | Semantic diff between git refs |
| `sqry explain <file> <symbol>` | Explain symbol with context and relations |
| `sqry similar <file> <symbol>` | Find similar symbols |
| `sqry subgraph <symbol>` | Extract focused subgraph |

### Visualization & Export

| Command | Description |
|---------|-------------|
| `sqry visualize <query>` | Generate diagrams (Mermaid, Graphviz, D2) |
| `sqry export` | Export graph (DOT, D2, Mermaid, JSON) |

### Session & Workflow

| Command | Description |
|---------|-------------|
| `sqry shell [path]` | Interactive REPL with warm cache |
| `sqry batch [path]` | Batch query execution from file |
| `sqry workspace` | Multi-repo workspace management |
| `sqry alias` | Query alias management |
| `sqry history` | Query history |
| `sqry config` | Configuration management |
| `sqry cache` | Cache management |
| `sqry insights` | Local usage insights |
| `sqry troubleshoot` | Diagnostic bundle generation |

### Server

| Command | Description |
|---------|-------------|
| `sqry lsp` | Start LSP server (`--stdio` for editors) |
| `sqry mcp setup` | Configure MCP server for AI assistants |
| `sqry completions <shell>` | Generate shell completions (bash, zsh, fish, powershell) |

## Query Syntax

Structured queries use predicates with boolean operators:

```bash
# By symbol kind
sqry query "kind:function"

# By name (regex)
sqry query "kind:function AND name~=/^handle/"

# By language
sqry query "kind:class AND lang:rust"

# By parent
sqry query "kind:method AND parent:MyClass"

# By visibility
sqry query "visibility:public AND kind:function"

# By async
sqry query "kind:function AND async:true"

# Combined
sqry query "(kind:function OR kind:method) AND lang:go AND name~=/error/"

# Explain without executing
sqry query "kind:function" --explain
```

## Output Formats

```bash
sqry main                    # Colored text (default)
sqry main --json             # JSON
sqry main --csv              # CSV (RFC 4180)
sqry main --tsv              # TSV
sqry main --count            # Count only
sqry main --no-color         # Plain text
```

## Filtering

```bash
sqry main --kind function    # By symbol type
sqry main --lang rust        # By language
sqry main --max-depth 3      # Directory depth
sqry main --exact            # Literal match
sqry main --fuzzy            # Fuzzy match
sqry main --ignore-case      # Case-insensitive
```

## Exit Codes

- `0` - Success (matches found)
- `1` - Error or no matches found

## Development

```bash
cargo build --package sqry-cli
cargo test --package sqry-cli
cargo run --package sqry-cli -- main src/
```

## Related Documentation

- [QUICKSTART.md](../QUICKSTART.md) - Getting started guide
- [Usage Examples](../docs/USAGE_EXAMPLES.md) - Detailed examples
- [Feature List](../docs/FEATURE_LIST.md) - Complete feature reference
- [Troubleshooting](../docs/TROUBLESHOOTING.md) - Common issues

## License

MIT - See [LICENSE-MIT](../LICENSE-MIT)

**Version**: 4.9.0
