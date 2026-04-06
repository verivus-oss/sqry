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
- [Communication](#communication)

---

## Project Overview

sqry is a semantic code search tool built in Rust that understands code structure through AST analysis. It supports 37 languages (28 with full relation extraction, 9 with symbol extraction + imports), provides CLI, LSP, and MCP interfaces, and runs entirely locally with no telemetry.

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

- Feature bloat (metrics exporters, language-specific linters)
- Features that don't serve semantic code search
- Changes that break the plugin architecture
- Complexity without clear value

---

## Development Setup

### Prerequisites

- **Rust 1.90+** with Edition 2024 (hard requirement)
- **Git** for version control

```bash
# Install or update Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup update stable

# Verify version (must be 1.90+)
rustc --version
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
├── sqry-nl/                # Natural language query translation
├── sqry-lang-*/            # 37 language plugins
├── sqry-lang-support/      # Plugin infrastructure
├── sqry-tree-sitter-support/ # Tree-sitter bindings
├── sqry-test-support/      # Test utilities
├── sqry-test-fixtures/     # Shared test fixtures
├── benchmarks/             # Performance benchmarks
├── crates/                 # Custom tree-sitter grammars
├── docs/                   # Documentation
│   ├── development/        # Per-feature development docs
│   ├── templates/          # Process document templates
│   └── user-guide/         # User-facing docs
├── test-fixtures/          # Language-specific test code
└── .github/workflows/      # CI pipelines
```

### Core Crates

| Crate | Purpose |
|-------|---------|
| `sqry-core` | Graph architecture, symbol types, search engine, query parser, plugin system |
| `sqry-cli` | CLI commands (`sqry index`, `sqry query`, `sqry graph`, etc.) |
| `sqry-lsp` | LSP server with 27 custom handlers (hover, definition, references, call hierarchy) |
| `sqry-mcp` | MCP server with 34 tools for AI assistants |

### Language Plugins

Each `sqry-lang-*` crate implements the `GraphBuilder` trait to extract symbols and relationships from source code via tree-sitter AST parsing.

**Full relation support (28)**: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig

**Symbol extraction + imports (7)**: Oracle PL/SQL, Salesforce Apex, SAP ABAP, ServiceNow Xanadu, Terraform, Puppet, Pulumi

---

## Development Process

### Process Selection

| Change Type | Documentation Required |
|-------------|----------------------|
| Bug fix (<50 LOC) | `04_PROGRESS-_SLUG_.md` + `06_TEST_EXECUTION-_SLUG_.md` entry |
| Documentation updates | None |
| Test additions | None |
| Language plugin | 3-doc pack (SPEC/IMPL/TESTS) |
| Feature (>50 LOC) | Full 6-doc pack |
| Architecture change | Full 6-doc pack |

### Feature Proposals

For major features (>50 LOC), open an issue or PR with:

1. **Spec** - What and why (requirements, goals)
2. **Design** - How (architecture, data structures)
3. **Test plan** - How you'll verify the implementation

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

Key steps within the **Test** job: malformed input tests (FFI safety across 35 plugin suites) and code quality checks.

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

Plugins emit nodes with `NodeKind` variants (28 total): `Function`, `Method`, `Class`, `Interface`, `Trait`, `Module`, `Variable`, `Constant`, `Type`, `Struct`, `Enum`, `EnumVariant`, `Macro`, `Parameter`, `Property`, `CallSite`, `Import`, `Export`, `StyleRule`, `StyleAtRule`, `StyleVariable`, `Lifetime`, `Component`, `Service`, `Resource`, `Endpoint`, `Test`, `Other`.

### Edge Kinds

Plugins emit edges including: `Defines`, `Contains`, `Calls`, `References`, `Imports`, `Exports`, `Inherits`, `Implements`, `TypeOf`, `FfiCall`, `HttpRequest`, and more.

### Getting Started

1. Study an existing plugin close to your target language (e.g., `sqry-lang-go` for a compiled language, `sqry-lang-python` for a dynamic language)
3. Add the crate to the workspace `members` list in the root `Cargo.toml`
4. Register the plugin in `sqry-plugin-registry/src/lib.rs` (the single source of truth for built-in plugins)
5. Add test fixtures in `test-fixtures/<language>/`
6. Write tests for symbol extraction and relationship detection

---

## Communication

### Asking Questions

- **GitHub Issues**: Bug reports and feature requests
- **Pull Requests**: Code contributions and design discussions
- **Discussions**: General questions and ideas

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
- [docs/USAGE_EXAMPLES.md](docs/USAGE_EXAMPLES.md) - Usage examples
- [docs/FEATURE_LIST.md](docs/FEATURE_LIST.md) - Complete feature list

---

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
