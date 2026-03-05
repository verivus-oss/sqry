# GEMINI.md

This file provides guidance to Gemini CLI when working with code in this repository.

For Codex and Claude-specific guidance, see [CODEX.md](CODEX.md) and [CLAUDE.md](CLAUDE.md).
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

## Gemini Context and Memory

- Gemini CLI's default instruction/context filename is `GEMINI.md`.
- Context is loaded hierarchically; use `/memory show` to inspect active context and `/memory refresh` to reload files.
- If you want AGENTS-style naming, configure `context.fileName` in Gemini settings.

Example (`~/.gemini/settings.json` or `.gemini/settings.json`):

```json
{
  "context": {
    "fileName": ["AGENTS.md", "GEMINI.md"]
  }
}
```

## Gemini MCP Notes

- Gemini MCP config is JSON settings-based (`~/.gemini/settings.json` / `.gemini/settings.json`).
- For sqry integration, use:

```bash
sqry mcp setup --tool gemini
sqry mcp status
```

- sqry's Gemini setup uses global config + CWD-based workspace discovery, so launch Gemini from the target project root.

## Engineering Workflow

- Use `rg`/`rg --files` for search and discovery.
- Prefer targeted edits and avoid unrelated churn.
- Run the narrowest relevant tests first, then broader checks when touching shared paths.
- Keep vendor updates (`third-party/`, patched crates, grammar crates) isolated unless required by the task.
- In handoff notes, always report:
  - files changed
  - behavior impact
  - tests executed (or not executed)
