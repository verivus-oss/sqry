# Using sqry MCP with Codex CLI

**Status**: Supported via `sqry mcp setup`
**Applies to**: Codex CLI using `~/.codex/config.toml`

For Gemini workflow details, see [GEMINI_INTEGRATION.md](GEMINI_INTEGRATION.md).
For shared context/skill definitions across Codex, Claude, and Gemini, see [../docs/LLM_SKILLS_STANDARD.md](../docs/LLM_SKILLS_STANDARD.md).

## Overview

Codex is configured as a global MCP client entry, and sqry workspace selection is done via current working directory (CWD) discovery.

This means:
- `sqry mcp setup --tool codex` writes config once
- You start Codex from the project directory you want to analyze
- sqry resolves the workspace from CWD (or parent directories)

## Quick Setup

```bash
# From your sqry repository
cargo install --path sqry-cli
cargo install --path sqry-mcp

# From the project you want to analyze
cd /path/to/project
sqry index .

# Configure Codex MCP entry
sqry mcp setup --tool codex

# Verify status
sqry mcp status
sqry mcp status --json | jq '.tools.codex'
```

## What `sqry mcp setup` Writes

Codex config file:
- `~/.codex/config.toml`

Expected entry:

```toml
[mcp_servers.sqry]
command = "/absolute/path/to/sqry-mcp"
```

Notes:
- Codex config is global.
- The default setup intentionally avoids pinning `SQRY_MCP_WORKSPACE_ROOT`.
- Start Codex from the target repository directory.

## Manual Configuration

If you need to configure by hand, add the same entry to `~/.codex/config.toml`:

```toml
[mcp_servers.sqry]
command = "/absolute/path/to/sqry-mcp"
```

Then validate:

```bash
sqry mcp status
```

## Workspace Behavior

Codex uses CWD-based workspace discovery by default.

Recommended workflow for multiple repositories:

```bash
cd /repo-a
codex

cd /repo-b
codex
```

If Codex is launched outside a project directory, sqry may fail to resolve `.sqry/graph` and return workspace resolution errors.

## Troubleshooting (Codex)

### `sqry mcp status` shows "not detected"

Create the entry via setup:

```bash
sqry mcp setup --tool codex
```

### `~/.codex/config.toml` parse errors

Fix invalid TOML first, then rerun setup:

```bash
sqry mcp setup --tool codex --force
```

### Wrong repository results

Launch Codex from the correct project root and confirm:

```bash
pwd
sqry index --status .
```
