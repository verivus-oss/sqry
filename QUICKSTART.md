# sqry Quick Start Guide - by Verivus

**Version**: 4.10.11
**Rust**: 1.90+ (Edition 2024)

---

## Install & Build

### Windows

```powershell
irm https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1 | iex
```

Default behavior resolves the latest GitHub release. For a pinned install, download the script and run `.\install.ps1 -Version vX.Y.Z`.

Review-first/signed variant:

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
.\install.ps1 -VerifySignatures
```

Manual fallback:
- download `sqry-windows-x86_64.zip`
- extract `sqry.exe`, `sqry-mcp.exe`, `sqry-lsp.exe`
- place them in a directory on `PATH`

### macOS / Linux

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
```

Published macOS binaries currently target Apple Silicon (`arm64`) only.

### Build from source

```bash
git clone https://github.com/verivus-oss/sqry.git
cd sqry

# Build everything
cargo build --workspace

# Verify
./target/debug/sqry --version
./target/debug/sqry --help

# Install to PATH (required for examples below)
cargo install --path sqry-cli
cargo install --path sqry-mcp    # MCP server binary
cargo install --path sqry-lsp    # LSP server binary
```

> **Note**: The examples below assume `sqry` is on your `PATH`. If you skip the install step, prefix commands with `./target/debug/` (e.g., `./target/debug/sqry index`).

### Requirements

- Rust 1.90+ with Edition 2024 (`rustup update stable`)
- ~20 GB disk for full build (35 tree-sitter grammars)

---

## Index a Codebase

```bash
# Index the current directory (creates .sqry/graph/snapshot.sqry)
sqry index

# Index a specific path
sqry index /path/to/project

# Check index status
sqry graph stats
```

---

## Core Commands

### Pattern Search

Fast regex-based symbol search:

```bash
# Find symbols matching a pattern
sqry search "test.*"

# Exact match only
sqry search --exact "main"

# Fuzzy search (requires index)
sqry search "config" --fuzzy
```

### Structured Query

Search by what code **means** using AST predicates and boolean logic:

```bash
# Find all public Rust functions
sqry query "kind:function AND visibility:public AND lang:rust"

# Find classes or structs
sqry query "kind:class OR kind:struct"

# Find async functions
sqry query "kind:function AND async:true"

# Relation queries: who calls authenticate?
sqry query "callers:authenticate"

# Hierarchical search (RAG-optimized, grouped by file)
sqry hier "kind:function visibility:public"
```

### Relations

Trace how code connects:

```bash
# Who calls this function?
sqry graph direct-callers parse_config

# What does this function call?
sqry graph direct-callees main

# Full call hierarchy (up and down)
sqry graph call-hierarchy parse_config --depth 3

# Relation queries via structured query
sqry query "callers:authenticate"
sqry query "imports:database"
```

### Graph Analysis

```bash
# Find circular dependencies
sqry cycles

# Find duplicate code
sqry duplicates

# Find unused symbols (dead code)
sqry unused

# Dependency impact analysis
sqry impact parse_config

# Trace path between two symbols
sqry graph trace-path parse_config main

# Extract focused subgraph
sqry subgraph parse_config --depth 2

# Semantic diff between git refs
sqry diff HEAD~1 HEAD

# Complexity metrics
sqry graph complexity
```

### Natural Language

```bash
# Ask questions about the codebase in plain English
sqry ask "What functions handle error recovery?"

# With automatic command execution
sqry ask "Find all public structs" --auto-execute
```

### Code Explanation

```bash
# Explain a symbol with full context
sqry explain parse_config

# Find similar symbols
sqry similar parse_config
```

### Export & Visualization

```bash
# Export graph in multiple formats
sqry export --format dot           # Graphviz DOT
sqry export --format d2            # D2 diagram
sqry export --format mermaid       # Mermaid
sqry export --format json          # JSON

# Visualize call relationships
sqry visualize "callers:main" --format mermaid

# Codebase insights
sqry insights
```

### Cache Management

```bash
sqry cache stats                   # View cache statistics
sqry cache prune --days 30         # Prune old entries
```

### Interactive Shell

```bash
# Start interactive REPL
sqry shell

# Batch mode (pipe commands from file)
sqry batch commands.txt
```

---

## MCP Server (AI Assistant Integration)

sqry includes a Model Context Protocol server with 33 tools for AI assistants:

```bash
# Start MCP server (stdio transport)
sqry-mcp

# Auto-configure for Claude Code, Codex, or Gemini CLI
sqry mcp setup

# Check MCP configuration status
sqry mcp status

# Use with Claude Code (add to .claude.json or ~/.claude.json):
# "mcpServers": { "sqry": { "command": "sqry-mcp", "args": [] } }
```

If your MCP client sends requests to hosted/external LLM providers, enable response sanitization with [sqry-mcp-redaction](sqry-mcp-redaction/README.md).

---

## LSP Server (Editor Integration)

```bash
# Start LSP server
sqry lsp

# Configure in your editor's LSP settings
# Example for VS Code: see sqry-vscode extension
```

---

## Language Support

**35 languages** across three tiers:

**General-purpose (28)**: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig

**Domain-specific (4)**: Oracle PL/SQL, Salesforce Apex, SAP ABAP, ServiceNow Xanadu

**IaC (3)**: Terraform, Puppet, Pulumi

---

## Project Structure

```
sqry/
├── sqry-core/              # Core library (graph, symbols, search, plugin system)
├── sqry-cli/               # CLI binary ('sqry')
├── sqry-lsp/               # LSP server
├── sqry-mcp/               # MCP server (AI assistant integration)
├── sqry-nl/                # Natural language translation
├── sqry-plugin-registry/   # Plugin registration
├── sqry-lang-*/            # 35 language plugins
├── test-fixtures/          # Shared test fixtures
└── supply-chain/           # cargo-vet audit data
```

---

## Development Workflow

### Build & Test

```bash
cargo build --workspace                              # Build
cargo test --workspace                               # Tests (REQUIRED before commit)
cargo fmt --all                                      # Format (REQUIRED)
cargo clippy --all-targets --workspace -- -D warnings  # Lint
```

### Commit Convention

```bash
# Format: <type>(<scope>): <subject>
git commit -m "feat(graph): add cross-file resolution"
git commit -m "fix(cache): prevent corruption on concurrent access"
```

Types: `feat` (MINOR bump), `fix` (PATCH), `perf` (PATCH), `refactor`, `docs`, `test`, `chore`

### Testing Checklist

```bash
cargo test --workspace           # All tests pass (24,000+)
cargo fmt --all --check          # Code formatted
cargo clippy --all-targets --workspace -- -D warnings  # No warnings
cargo build --workspace          # Build succeeds
```

---

## Troubleshooting

```bash
# Clean rebuild
cargo clean && cargo build --workspace

# Check Rust version (need 1.90+)
rustc --version

# Run specific test
cargo test -p sqry-core test_name

# Run with output
cargo test -- --nocapture

# Generate diagnostic bundle
sqry troubleshoot
```

---

## Further Reading

- **CLAUDE.md** — Repository guide and architecture
- **CONTRIBUTING.md** — How to contribute
- **CHANGELOG.md** — Release history
- **CONFIGURATION_TUNING_GUIDE.md** — Performance tuning
- **ERRATA.md** — Known issues and corrections
