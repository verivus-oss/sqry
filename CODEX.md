# CODEX.md

This file provides guidance to Codex CLI (OpenAI Codex) when working with code in this repository.

For Claude and Gemini-specific guidance, see [CLAUDE.md](CLAUDE.md) and [GEMINI.md](GEMINI.md).
For shared context/skill definitions across all three agents, see [docs/LLM_SKILLS_STANDARD.md](docs/LLM_SKILLS_STANDARD.md).

## Project Overview

sqry is a semantic code search engine built in Rust that understands code structure through AST analysis (not embeddings). It parses source code with tree-sitter, builds a unified graph of symbols and relationships, and provides fast queries via CLI, LSP, and MCP interfaces.

The built-in plugin registry currently includes 35 language plugins.

## Build and Development Commands

Run commands from the `sqry/` workspace root.

```bash
# Build
cargo build --workspace

# Test (full workspace - required before PR)
cargo test --workspace

# Test single crate
cargo test -p sqry-core
cargo test -p sqry-lang-rust

# Test single test by name
cargo test -p sqry-core test_name

# Test with output visible
cargo test -- --nocapture

# Run ignored/slow tests
cargo test -- --ignored

# Quality gates (all should pass before PR)
cargo fmt --all
cargo clippy --all-targets --workspace -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
```

Rust requirement: `1.90+` with Edition `2024` (hard requirement).

### Verbose Test Logging

```bash
SQRY_TEST_VERBOSE=all cargo test -- --nocapture
SQRY_TEST_VERBOSE=core,cli cargo test -- --nocapture
SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_LEVEL=trace cargo test -- --nocapture
```

Log artifacts are written to `target/test-artifacts/<crate>/<timestamp>.log`.

### Benchmarks

```bash
cargo bench -p sqry-core --bench <bench_name>
```

## Codex Workflow Notes

- Use `rg`/`rg --files` for search and discovery.
- Prefer targeted edits and avoid unrelated churn.
- Run the narrowest relevant tests first, then broader checks when touching shared paths.
- Keep vendor updates (`third-party/`, patched crates, grammar crates) isolated unless required by the task.
- In handoff notes, always report:
  - files changed
  - behavior impact
  - tests executed (or not executed)

## Architecture

### Workspace Crate Hierarchy

```text
sqry-core               Core library: graph, symbols, search engine, query parser, plugin system, cache
sqry-cli                CLI binary ("sqry") - index, query, graph, visualize commands
sqry-lsp                LSP server (hover, definition, references, call hierarchy)
sqry-mcp                MCP server (tool surface for AI assistants, JSON-RPC over stdio)
sqry-nl                 Natural language -> sqry query translation (optional classifier)
sqry-plugin-registry    Single source of truth for built-in language plugin registration
sqry-lang-support       Shared helpers for language plugins
sqry-lang-*             Language plugins (one crate per language)
sqry-mcp-redaction      Client-side MCP response redaction library
sqry-tree-sitter-support Tree-sitter binding helpers
sqry-test-support       Test infrastructure (verbose logging, artifacts)
sqry-test-fixtures      Shared fixture data
```

### Key Abstractions

Plugin system (`sqry-core/src/plugin/`):
- `LanguagePlugin` is implemented by language crates.
- `PluginManager` handles registration and extension-based lookup.
- Built-ins are registered in `sqry-plugin-registry/src/lib.rs`.

GraphBuilder trait (`sqry-core/src/graph/builder.rs`):
- Each plugin implements `GraphBuilder::build_graph()`.
- Builders receive a tree-sitter `Tree` and populate a staging graph with nodes and edges.

Unified graph (`sqry-core/src/graph/unified/`):
- Arena + compressed sparse row style storage with generational indices.
- Supports concurrent read-heavy workloads with controlled write paths.

Graph build pipeline (`sqry-core/src/graph/unified/build/`):
- Pass 3: Intra-file relation resolution
- Pass 4: Cross-file relation resolution
- Pass 5: Cross-language detection (for example TS->JS, Python->C FFI, HTTP, SQL)

Query engine (`sqry-core/src/query/`):
- Lexer -> parser -> optimizer -> plan -> execution pipeline
- Supports boolean logic and fielded queries such as `callers:X`, `impl:Trait`, `unused:true`

Search engine (`sqry-core/src/search/`):
- Fuzzy matching, trigram indexing, SIMD-accelerated search, ranking

### Node and Edge Kinds

Plugins emit `NodeKind` variants such as:
`Function`, `Method`, `Class`, `Interface`, `Trait`, `Module`, `Variable`, `Constant`, `Type`, `Struct`, `Enum`, `EnumVariant`, `Macro`, `Import`, `Export`, `Component`, `Service`, `Resource`, `Endpoint`, `Test`, `Other`.

Common edge kinds include:
`Defines`, `Contains`, `Calls`, `References`, `Imports`, `Exports`, `Inherits`, `Implements`, `TypeOf`, `FfiCall`, `HttpRequest`.

## Adding a New Language Plugin

1. Create `sqry-lang-<language>/` implementing the plugin and graph builder.
2. Add it to workspace `members` in root `Cargo.toml`.
3. Register it in `sqry-plugin-registry/src/lib.rs`.
4. Add fixtures in `test-fixtures/<language>/`.
5. Add symbol/relation tests and malformed input tests.

## Code Conventions

- Error handling:
  - CLI code: `anyhow::Result` plus `.context(...)`
  - Library code: typed `thiserror` enums
- Avoid panics in library code.
- Memory and concurrency:
  - use `Arc<str>` and interning for repeated symbols
  - use `rayon` for parallelism and `parking_lot` for synchronization
- Formatting:
  - `rustfmt.toml` (max width 100, 4 spaces, unix newlines)
- Lints:
  - clippy is enforced with `-D warnings`

## CI Expectations

CI validates build/tests and quality gates across major platforms. Keep changes deterministic and test-covered, especially for:
- plugin registration and extraction behavior
- graph/query execution paths
- CLI/LSP/MCP integration behavior

## Vendored and Patched Dependencies

Patched crates are mapped in root `Cargo.toml` via `[patch.crates-io]`, including:
- `ssri`
- `lsp-types`
- `kqueue-sys`
- `tower-lsp`
- `jobserver`

Custom grammar crates are maintained under `crates/` and related vendored sources in `third-party/`.
