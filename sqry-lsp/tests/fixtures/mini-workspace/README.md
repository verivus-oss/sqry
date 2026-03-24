# Mini Workspace Fixture

Deterministic workspace for Phase 2 LSP integration tests.

## Contents

- `src/lib.rs` — Rust module with nested functions, doc comments, and emoji identifiers for UTF-16 coverage
- `src/utils.ts` — TypeScript utilities including overloaded functions and multibyte characters

## Symbol Map

| File | Symbol | Kind | Notes |
|------|--------|------|-------|
| lib.rs | `process_data` | fn | Public async function with doc comment |
| lib.rs | `InnerState` | struct | Nested struct used by `process_data` |
| lib.rs | `emoji_fn` | fn | Function named with emoji identifier to test UTF-16 |
| utils.ts | `transform` | function | Default export used by Rust module via comment reference |
| utils.ts | `describe` | function overload | Includes characters with combining marks |

Positions assumed in tests: see inline comments in source files.
