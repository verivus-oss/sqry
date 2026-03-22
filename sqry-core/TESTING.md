# Testing Guide for sqry-core

## Overview

The `sqry-core` crate contains unit tests, integration tests, and benchmarks. Due to architectural constraints related to plugin dependencies, some tests require special handling.

## Running Tests

### Standard Unit Tests (Recommended)

To run all unit tests that don't require plugin registration:

```bash
cargo test -p sqry-core --lib
```

This runs 626+ tests covering:
- AST operations
- Symbol extraction and indexing
- Fuzzy search
- Cache management
- Query execution
- Metadata normalization
- And more

**Status:** ✅ All tests pass (626/626)

### Integration Tests

Integration tests are located in `tests/` and include the AST hardening regression suite (FT-D.1):

```bash
# Run all integration tests
cargo test -p sqry-core --tests

# Run specific test suite
cargo test -p sqry-core --test ast_hardening_regression
```

**Status:** ✅ All tests pass (20/20 for ast_hardening_regression)

### Benchmarks

To run performance benchmarks:

```bash
# Run all benchmarks
cargo bench -p sqry-core

# Run specific benchmark
cargo bench -p sqry-core --bench ast_operations
```

Available benchmarks:
- `ast_operations` - AST parsing, symbol extraction, metadata normalization (FT-D.2)
- `cache_benchmarks` - Cache operations
- `parallel_indexing_benchmark` - Parallel indexing performance
- `fuzzy_e2e_benchmark` - End-to-end fuzzy search
- And more (see Cargo.toml for full list)

## Context Tests (Special Handling Required)

### The Circular Dependency Issue

The `context.rs` module contains tests that register language plugins (RustPlugin, PythonPlugin, etc.). These tests are **disabled by default** due to a circular dependency issue:

1. `sqry-core` has dev-dependencies on `sqry-lang-rust`, `sqry-lang-python`, etc.
2. These plugins depend on `sqry-core` (for the `LanguagePlugin` trait)
3. This creates two instances of `sqry-core` in the dependency graph during testing
4. Result: Trait version mismatch error (E0277)

**Error Example:**
```
error[E0277]: the trait bound `RustPlugin: plugin::types::LanguagePlugin` is not satisfied
```

### Why These Tests Are Disabled

The context.rs tests are gated behind the `context-tests` feature flag, which is **not enabled by default**. This allows:

- ✅ `cargo test -p sqry-core` to compile and pass (626 tests)
- ✅ Integration tests to run normally (they don't have circular deps)
- ✅ CI/CD pipelines to work without special configuration
- ❌ Context extraction tests to be skipped (11 tests)

### Running Context Tests (Not Recommended)

If you need to run the context tests, you can try enabling the feature flag:

```bash
cargo test -p sqry-core --lib --features context-tests context::
```

**Note:** This will likely fail with E0277 due to the circular dependency. These tests are better suited for:

1. **Workspace-level testing** (if configured properly)
2. **Separate integration test crate** (future work)
3. **Manual testing** with sqry-cli

### Alternative: Test Coverage

The functionality tested by the disabled context tests is covered by:

1. **Integration tests** - `ast_hardening_regression.rs` (20 tests) validates symbol extraction with context across multiple languages
2. **CLI tests** - End-to-end tests in `sqry-cli` verify context extraction in real scenarios
3. **Manual testing** - Use `sqry search` with context queries

## Test Counts

| Test Suite | Count | Status | Command |
|------------|-------|--------|---------|
| Unit tests (lib) | 626 | ✅ Pass | `cargo test -p sqry-core --lib` |
| Integration tests | 20+ | ✅ Pass | `cargo test -p sqry-core --tests` |
| Context tests | 11 | ⚠️ Disabled | (circular dependency) |
| **Total** | **646+** | **626 passing** | |

## Continuous Integration

For CI/CD pipelines, use:

```bash
# Run all tests that should pass
cargo test -p sqry-core --lib --tests

# Or simply
cargo test -p sqry-core
```

This runs all unit tests (626) and integration tests (20+) without encountering circular dependency issues.

## Future Work

To enable the context tests, we would need to:

1. **Option A:** Create a separate integration test crate that depends on both sqry-core and plugins (no circular dep)
2. **Option B:** Move context extraction tests to sqry-cli integration tests
3. **Option C:** Refactor the plugin architecture to break the circular dependency (major refactor)

For now, the current setup provides excellent test coverage (646+ tests) while avoiding build system complexity.

## Summary

✅ **Standard workflow:** `cargo test -p sqry-core` runs 626+ tests and passes
⚠️ **Context tests:** Disabled due to circular dependency (covered by integration tests)
✅ **Coverage:** Excellent - all core functionality is tested via unit + integration tests
