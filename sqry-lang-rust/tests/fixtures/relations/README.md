# Rust Relations Test Fixtures

This directory contains test fixtures for validating Rust relation extraction behavior.

## Purpose

These fixtures establish a baseline of current behavior before migrating to `relations-shared`. Differential tests compare outputs before and after migration to ensure zero behavioral changes.

## Fixture Files

### imports.rs
Tests all 5 import patterns:
1. Scoped with alias: `use std::collections::HashMap as Map`
2. Simple with alias: `use std::io as StdIo`
3. Scoped without alias: `use std::sync::Arc`
4. Simple without alias: `use tokio`
5. Grouped imports: `use std::path::{Path, PathBuf}`

### calls.rs
Tests three callee types:
1. Simple identifier: `foo()`
2. Field expression: `obj.method()`
3. Scoped identifier: `std::io::stdout()`

Also tests:
- Method calls with self receiver
- Nested function calls
- Mixed call patterns

### exports.rs
Tests visibility-based export detection:
- `pub` items (should be exported)
- `pub(crate)` items (document actual behavior)
- Private items (should NOT be exported)

Also tests:
- Multiple export types (functions, structs, enums, traits)
- Impl blocks with public methods
- Public items in private modules

## Usage

These fixtures are used by `tests/relations_differential.rs` to generate snapshot baselines using the `insta` crate.

## Maintenance

When Rust plugin behavior intentionally changes, update fixtures and regenerate snapshots with:

```bash
cargo test relations_differential
cargo insta review
```
