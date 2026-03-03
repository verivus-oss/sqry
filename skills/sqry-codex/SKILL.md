---
name: sqry-codex
description: Use when working in this repository with OpenAI Codex and you need the canonical Codex workflow, commands, and constraints.
---

# sqry Codex Guide

Use this skill when the active agent is Codex for implementation, review, or repository maintenance in `sqry`.

## Primary references

- [CODEX.md](../../CODEX.md)
- [AGENTS.md](../../AGENTS.md)
- [README.md](../../README.md)
- [sqry-mcp/README.md](../../sqry-mcp/README.md)

## Workflow

1. Read `CODEX.md` first for repo conventions and expected delivery format.
2. Use `rg` and targeted file reads before editing.
3. Run the narrowest relevant tests first, then broader workspace checks for shared paths.
4. Report files changed, behavior impact, and tests run (or not run) in handoff.

## Baseline commands

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets --workspace -- -D warnings
```

## Notes

- Run commands from the workspace root.
- Keep vendored or grammar-crate changes isolated unless the task requires them.
- Use `sqry-mcp/README.md` and `sqry-mcp/CODEX_INTEGRATION.md` for Codex MCP setup and troubleshooting.
