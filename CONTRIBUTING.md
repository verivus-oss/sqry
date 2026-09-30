# Contributing to sqry

Thank you for your interest in contributing to sqry! This guide covers everything you need to get started.

## Table of Contents

- [Project Overview](#project-overview)
- [Core Philosophy](#core-philosophy)
- [Development Setup](#development-setup)
- [Workspace Structure](#workspace-structure)
- [Development Process](#development-process)
- [Code Style Guide](#code-style-guide)
- [Testing](#testing)
- [Pull Request Process](#pull-request-process)
- [Adding a Language Plugin](#adding-a-language-plugin)
- [Community Posture](#community-posture)
- [Communication](#communication)
- [Additional Resources](#additional-resources)
- [License](#license)

---

## Project Overview

sqry is a semantic code search tool built in Rust that understands code structure through AST analysis. It provides CLI, LSP, and MCP interfaces, and runs entirely locally with no telemetry.

Counts of languages and tools are deliberately not written down here. They move every release and a stale number in a contributor guide is worse than no number. Ask the build you have:

```bash
sqry --list-languages      # languages the installed binary enables
sqry-mcp --list-tools      # MCP tools the installed server exposes
```

The `sqry://meta/manifest` MCP resource reports the same figures for a connected server.

---

## Core Philosophy

> **"Do one thing exceptionally well - semantic code search."**

Every contribution must pass the **Semantic Search Litmus Test**:
> "Does this make sqry better at semantic code search?"

### What We Welcome

- Core search functionality improvements
- AST/symbol extraction enhancements
- Language plugin development
- Documentation and examples
- Performance optimizations
- Bug fixes
- Test coverage improvements

### What We Won't Accept

- Features that do not serve semantic code search
- Changes that break the plugin architecture
- Complexity without clear value

The litmus test is about the graph, not about a category name. sqry does emit
Prometheus-format index status (`sqry index --status --metrics-format prometheus`)
and does run declarative rule packs (`sqry rules`), both because they answer
questions about the graph it already builds. An earlier version of this list
rejected "metrics exporters" and "language-specific linters" outright while both
of those shipped. If a proposal reads like one of those, argue it against the
litmus test rather than against the label.

---

## Development Setup

### Prerequisites

- **Rust**, Edition 2024. The exact toolchain is pinned in `rust-toolchain.toml`, and
  rustup honours it automatically inside the repository, so you do not need to pick a
  version yourself.
- **Git** for version control

```bash
# Install rustup if you do not have it
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Inside the repo, this reports the pinned toolchain, not your default
rustc --version
cat rust-toolchain.toml
```

### Initial Setup

```bash
# Clone the repository
git clone https://github.com/verivus-oss/sqry.git
cd sqry

# Build all crates
cargo build --workspace

# Run all tests
cargo test --workspace

# Build documentation
cargo doc --workspace --no-deps --open
```

### Quality Gates

These must all pass before submitting a PR:

```bash
cargo fmt --all                                        # Format
cargo clippy --all-targets --workspace -- -D warnings   # Lint (zero warnings)
cargo test --workspace                                  # Tests
cargo doc --workspace --no-deps                         # Documentation builds
```

All clippy warnings must be resolved before merge.

---

## Workspace Structure

```
sqry/
├── sqry-core/              # Core library: graph, symbols, search, plugin system
├── sqry-cli/               # CLI binary (sqry)
├── sqry-lsp/               # LSP server (sqry lsp)
├── sqry-mcp/               # MCP server for AI assistants
├── sqry-daemon/            # Daemon binary and library (sqryd)
├── sqry-daemon-protocol/   # Daemon wire types and framing
├── sqry-daemon-client/     # Daemon client library
├── sqry-db/                # Derived-analysis cache and query planner
├── sqry-classpath/         # JVM classpath analysis
├── sqry-rules/             # Declarative rule layer
├── sqry-mcp-redaction/     # MCP response redaction
├── sqry-plugin-registry/   # Plugin registration and discovery
├── sqry-lang-*/            # Language plugins
├── sqry-lang-support/      # Plugin infrastructure
├── sqry-tree-sitter-support/ # Tree-sitter bindings
├── sqry-vscode/            # VS Code extension
├── agent-skills/           # Consumer agent skills and plugin manifests
├── crates/                 # Vendored tree-sitter grammars
├── docs/user-guide/        # User-facing documentation
├── test-fixtures/          # Language-specific test code
├── tests/                  # Workspace-level integration tests
└── .github/workflows/      # CI pipelines
```

`cargo metadata --no-deps --format-version 1` is the authoritative list; the tree above
is a map, not an inventory.

### Core Crates

| Crate | Purpose |
|-------|---------|
| `sqry-core` | Graph architecture, symbol types, search engine, query parser, plugin system |
| `sqry-cli` | CLI commands (`sqry index`, `sqry query`, `sqry graph`, etc.) |
| `sqry-lsp` | LSP server (hover, definition, references, call hierarchy, and custom handlers) |
| `sqry-mcp` | MCP server for AI assistants; run `sqry-mcp --list-tools` for the current catalog |

### Language Plugins

Each `sqry-lang-*` crate implements the `GraphBuilder` trait to extract symbols and relationships from source code via tree-sitter AST parsing.

Plugins differ in depth. Some extract symbols and full relations; others extract symbols
and imports only. Some are compiled by default and some sit behind Cargo features.

Rather than repeat a breakdown that goes stale, read it off the build:

```bash
sqry --list-languages                      # what this binary enables
SQRY_INCLUDE_HIGH_COST=1 sqry --list-languages   # plus the high-cost plugins
```

`--include-high-cost` is a flag on `sqry index`, not on `--list-languages`, so
the environment variable is the form that works for listing.

Two earlier revisions of this file disagreed with each other about the split, one
paragraph saying nine symbol-and-imports languages and another saying seven and then
listing seven. That is the failure mode this section now avoids.

---

## Development Process

### Process Selection

What an outside contribution needs is a clear description of the change and evidence
that it works. Nothing more is asked of you here.

| Change Type | What to include in the PR |
|-------------|---------------------------|
| Bug fix | The failing case, and a test that fails without the fix |
| Documentation | The change, and how you checked the claim it makes |
| Test additions | What behaviour the test pins, and evidence it fails when that behaviour is removed |
| Language plugin | A short spec, the implementation, and per-construct tests |
| Feature | A short spec (what and why), a design sketch (how), and a test plan |
| Architecture change | The same, plus the migration story for existing indexes |

Earlier revisions of this file required a numbered six-document pack whose templates
live in `docs/templates/`, a directory that is not part of this repository. Asking an
outside contributor for documents they cannot see was a mistake, and the requirement
was internal-process residue rather than something this project needs from you.

---

## Code Style Guide

### Naming Conventions

| Element | Convention | Example |
|---------|-----------|---------|
| Types | `PascalCase` | `SymbolIndex`, `QueryExecutor` |
| Functions/methods | `snake_case` | `extract_symbols`, `parse_query` |
| Constants | `SCREAMING_SNAKE_CASE` | `MAX_FILE_SIZE` |
| Modules | `snake_case` | `symbol_extraction` |


### Error Handling

- **CLI code**: Use `anyhow::Result<T>` with `.context()` for user-facing errors
- **Library code**: Use `thiserror` with typed error enums
- **Never panic** in library code; use `Result<T, E>` for fallible operations

### Memory and Concurrency

- Use `Arc<str>` and string interning for repeated symbols
- Use `rayon` for parallelism, `parking_lot` for locks
- Avoid `tokio` unless the operation is IO-bound

### Documentation

- Document all `pub` items with `///` doc comments
- Use `//!` for module-level documentation
- Explain **why**, not **what** (code should be self-documenting)

---

## Testing

### Running Tests

```bash
# Full workspace (required before PR)
cargo test --workspace

# Single crate
cargo test -p sqry-core

# Single test
cargo test -p sqry-core test_name

# With output
cargo test -- --nocapture

# Ignored tests
cargo test -- --ignored
```

### Test Organization

| Category | Location | Description |
|----------|----------|-------------|
| Unit tests | Inline (`#[cfg(test)] mod tests`) | Fast, isolated, no I/O |
| Integration tests | `<crate>/tests/` | Component interactions, public APIs |
| Plugin tests | `sqry-lang-*/tests/` | Symbol extraction, relation tracking |
| E2E tests | `sqry-mcp/tests/` | Full MCP server with real graph |
| Malformed input tests | `sqry-lang-*/tests/malformed_input.rs` | FFI safety boundary |
| Benchmarks | `benchmarks/` | Performance regression detection |

### Test Requirements

- New features must have tests
- Bug fixes must include a regression test
- All tests must pass before PR submission
- Target >80% coverage, 100% for critical paths

### Writing Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_symbols_finds_functions() {
        let source = b"fn main() {}";
        let file = PathBuf::from("test.rs");

        let symbols = extract_symbols(source, &file).unwrap();

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].name, "main");
    }
}
```

### Verbose Test Logging

Enable detailed logging for debugging without modifying code:

```bash
# Enable for all crates
SQRY_TEST_VERBOSE=all cargo test -- --nocapture

# Enable for specific crates
SQRY_TEST_VERBOSE=core,cli cargo test -- --nocapture

# With trace-level detail
SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_LEVEL=trace cargo test -- --nocapture

# Capture logs to files for post-mortem analysis
SQRY_TEST_VERBOSE=all SQRY_TEST_VERBOSE_ARTIFACTS=1 cargo test
```

Log artifacts are written to `target/test-artifacts/<crate>/<timestamp>.log`.

---

## Pull Request Process

### Branch Naming

```bash
git checkout -b feat/description    # New features
git checkout -b fix/description     # Bug fixes
git checkout -b docs/description    # Documentation
git checkout -b refactor/description # Refactoring
```

### Commit Messages

Use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>
```

| Type | Use |
|------|-----|
| `feat` | New feature (MINOR version bump) |
| `fix` | Bug fix (PATCH version bump) |
| `docs` | Documentation only |
| `test` | Adding or updating tests |
| `refactor` | Code restructuring |
| `perf` | Performance improvement |
| `chore` | Maintenance tasks |
| `ci` | CI/CD pipeline changes |

**Breaking changes**: Add `BREAKING CHANGE:` in the commit footer (MAJOR version bump).

**Examples**:
```
feat(graph): add cross-file resolution for imports
fix(typescript): handle empty interface declarations
docs: update contributing guide with current project state
test(core): add cache multiprocess safety tests
```

### Before Submitting

1. Run all quality gates (fmt, clippy, test, doc)
2. Ensure new code has tests
3. Update documentation if applicable
4. Write a clear PR description explaining what and why

### PR Description Template

```markdown
## Summary

Brief description of what this PR does and why.

## Changes

- Key changes with affected components

## Testing

- How you tested the changes
- Test commands and results

## Breaking Changes

- List any breaking changes
```

### CI Pipeline

PRs trigger the following CI checks:

| Job | What it does |
|-----|-------------|
| **Test** | Build + tests on Ubuntu, macOS (13/14), Windows |
| **Clippy** | Lint with `-D warnings` (zero warnings) |
| **Rustfmt** | Formatting check |
| **Documentation** | `cargo doc` with `-D warnings` |
| **Security** | `cargo-audit` + `cargo-deny` |
| **Fuzzy Search** | Tests both Jaccard and ratio modes |
| **Unwrap Safety** | Advisory check for `unwrap`/`expect` usage |

Key steps within the **Test** job: malformed-input tests, which exercise FFI safety across the language plugins, and code quality checks. The suites are the `malformed_input.rs` files under `sqry-lang-*/tests/`; `find . -name malformed_input.rs | wc -l` is the count, and it moves with the plugin set.

All CI jobs must pass before merge.

---

## Adding a Language Plugin

Language plugins are the most common contribution type. Each plugin lives in its own crate (`sqry-lang-<language>/`) and implements the `GraphBuilder` trait.

### Plugin Architecture

Every plugin must:

1. **Parse source code** using tree-sitter
2. **Extract symbols** (functions, classes, methods, etc.) as graph nodes
3. **Extract relationships** (calls, imports, exports, etc.) as graph edges
4. **Handle malformed input** without panicking (FFI safety)

### Key Trait

```rust
pub trait GraphBuilder: Send + Sync {
    fn build_graph(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
        staging: &mut StagingGraph,
    ) -> GraphResult<()>;
}
```

### Node Kinds

Plugins emit nodes typed by the `NodeKind` enum and edges typed by `EdgeKind`. Both
grow, so read them from the source rather than from a list here:

```bash
sqry index .                                             # once, if you have not already
sqry query 'kind:enum name:NodeKind' sqry-core/src/graph
sqry query 'kind:enum name:EdgeKind' sqry-core/src/graph
```

Each returns two hits. The ones under `sqry-core/src/graph/unified/` are the current
definitions that plugins emit into; the pair directly under `sqry-core/src/graph/` are
the older types.

The index and the path scope both matter. Without an index `sqry query` currently aborts
rather than reporting that there is none (verivus-oss/sqry#829), and an unscoped
query over the whole workspace is what triggers it.

An earlier revision of this file listed 28 `NodeKind` variants while the enum had 35,
and that stale count survived a rewrite whose whole purpose was removing stale counts.
Hence the query rather than the list.

### Edge Kinds

Plugins emit edges including: `Defines`, `Contains`, `Calls`, `References`, `Imports`, `Exports`, `Inherits`, `Implements`, `TypeOf`, `FfiCall`, `HttpRequest`, and more.

### Getting Started

1. Study an existing plugin close to your target language (e.g., `sqry-lang-go` for a compiled language, `sqry-lang-python` for a dynamic language)
3. Add the crate to the workspace `members` list in the root `Cargo.toml`
4. Register the plugin in `sqry-plugin-registry/src/lib.rs` (the single source of truth for built-in plugins)
5. Add test fixtures in `test-fixtures/<language>/`
6. Write tests for symbol extraction and relationship detection

---

## Community Posture

Read this before investing time in a large change.

Issues and pull requests are open on the public repository, and outside contributions
are read. Development happens primarily in a private tree and is mirrored here on each
release, so a public pull request is landed by re-applying it internally rather than by
merging the branch directly. That means your commit may reach the released code under a
different SHA, and it means review latency is tied to the release cycle rather than to
the working day.

GitHub Discussions are not enabled. Use issues.

For anything larger than a bug fix, open an issue describing the change before writing
it, so the litmus test conversation happens before the work rather than after.

---

## Communication

### Asking Questions

- **GitHub Issues**: Bug reports and feature requests
- **Pull Requests**: Code contributions and design discussion
- **Issues**: Bug reports, feature proposals, and general questions

### Reporting Bugs

Include:

1. **Environment**: OS, Rust version (`rustc --version`), sqry version (`sqry --version`)
2. **Reproduction**: Minimal example that reproduces the bug
3. **Expected vs actual behavior**
4. **Error output**: Complete error messages or stack traces

### Suggesting Features

Include:

1. **Use case**: The problem you're solving
2. **Proposed solution**: How you envision it working
3. **Alternatives considered**: Other approaches
4. **Semantic search relevance**: How does this improve code search?

---

## Additional Resources

- [README.md](README.md) - Project overview and usage
- [QUICKSTART.md](QUICKSTART.md) - Quick start guide
- [docs/FEATURE_LIST.md](docs/FEATURE_LIST.md) - Complete feature list
- [docs/user-guide/](docs/user-guide/) - Task-oriented user documentation
- [SECURITY.md](SECURITY.md) - Reporting a vulnerability

Every path linked above resolves in the published repository, which is where this guide
is read. If you find one that does not, that is a bug in this file and worth an issue on
its own: it means the guide has drifted from the tree it ships with, which has happened
before. `SECURITY.md` is maintained directly on the published repository rather than
mirrored into it, so it is the one link here that will not resolve in a development
checkout.

---

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
