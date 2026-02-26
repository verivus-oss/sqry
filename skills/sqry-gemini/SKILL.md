---
name: sqry-gemini
description: Use when working in this repository with Gemini CLI and you need the canonical Gemini workflow, context-file behavior, and MCP setup details.
---

# sqry Gemini Guide

Use this skill when the active agent is Gemini CLI for implementation, review, or repository maintenance in `sqry`.

## Primary references

- [GEMINI.md](../../GEMINI.md)
- [AGENTS.md](../../AGENTS.md)
- [README.md](../../README.md)
- [sqry-mcp/README.md](../../sqry-mcp/README.md)

## Workflow

1. Read `GEMINI.md` first for repository workflow plus Gemini context/memory behavior.
2. Use `rg` for fast discovery and keep edits narrowly scoped.
3. Run focused tests first; run broader checks for shared graph, query, or plugin paths.
4. In handoff, always include changed files, impact summary, and tests run/not run.

## Baseline commands

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets --workspace -- -D warnings
```

## Notes

- Run commands from the workspace root.
- Gemini context defaults to `GEMINI.md`; `AGENTS.md` inclusion is optional via Gemini settings.
- Use `sqry-mcp/README.md` and `sqry-mcp/GEMINI_INTEGRATION.md` for Gemini MCP configuration and validation.
