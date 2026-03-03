---
name: sqry-claude
description: Use when working in this repository with Claude Code and you need the canonical Claude-specific workflow, commands, and architecture references.
---

# sqry Claude Guide

Use this skill when the active agent is Claude Code for implementation, review, or repository maintenance in `sqry`.

## Primary references

- [CLAUDE.md](../../CLAUDE.md)
- [AGENTS.md](../../AGENTS.md)
- [README.md](../../README.md)
- [sqry-mcp/README.md](../../sqry-mcp/README.md)

## Workflow

1. Read `CLAUDE.md` first for architecture map, coding conventions, and CI expectations.
2. Use targeted search (`rg`) and focused edits to avoid unrelated churn.
3. Validate changes with crate-scoped tests first, then workspace gates when shared code changes.
4. In handoff, state file changes, behavioral impact, and verification status.

## Baseline commands

```bash
cargo build --workspace
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets --workspace -- -D warnings
```

## Notes

- Run commands from the workspace root.
- Treat malformed input tests and plugin registration changes as high-risk and verify explicitly.
- Use `sqry-mcp/README.md` and `sqry-mcp/CLAUDE_CODE_INTEGRATION.md` for Claude-specific MCP usage.
