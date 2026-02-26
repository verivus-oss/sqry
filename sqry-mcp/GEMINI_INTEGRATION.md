# Using sqry MCP with Gemini CLI

**Status**: Supported via `sqry mcp setup`
**Applies to**: Gemini CLI using `~/.gemini/settings.json` (or project `.gemini/settings.json`)

For cross-agent context/skill definitions, see [../docs/LLM_SKILLS_STANDARD.md](../docs/LLM_SKILLS_STANDARD.md).

## Overview

Gemini can load sqry through MCP server config in `settings.json` under `mcpServers`.

For sqry's auto-setup:
- `sqry mcp setup --tool gemini` writes Gemini MCP config
- Gemini workspace selection is CWD-based by default for sqry integration
- Launch Gemini from the repository you want to analyze

## Quick Setup

```bash
# From your sqry repository
cargo install --path sqry-cli
cargo install --path sqry-mcp

# From the project you want to analyze
cd /path/to/project
sqry index .

# Configure Gemini MCP entry
sqry mcp setup --tool gemini

# Verify status
sqry mcp status
sqry mcp status --json | jq '.tools.gemini'
```

## What `sqry mcp setup` Writes

Gemini config file:
- `~/.gemini/settings.json`

Expected entry:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

Notes:
- Gemini config is global unless you maintain a project-level `.gemini/settings.json`.
- sqry setup does not pin `SQRY_MCP_WORKSPACE_ROOT` for Gemini by default.
- Start Gemini from the target repository directory.

## Manual Configuration

If you need to configure by hand, add this entry in `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

Then validate:

```bash
sqry mcp status
```

## Optional: Load AGENTS.md with GEMINI.md

Gemini defaults to `GEMINI.md`, but you can include `AGENTS.md` in the context filename list:

```json
{
  "context": {
    "fileName": ["AGENTS.md", "GEMINI.md"]
  }
}
```

Use `/memory refresh` after editing context files.
Use `/memory show` to verify the effective merged context.

This repository now includes a project-scoped example at `.gemini/settings.json`.

## Troubleshooting (Gemini)

### `sqry mcp status` shows Gemini not configured

```bash
sqry mcp setup --tool gemini
```

### `~/.gemini/settings.json` parse errors

Fix invalid JSON first, then rerun setup:

```bash
sqry mcp setup --tool gemini --force
```

### Wrong repository results

Launch Gemini from the intended project root and confirm:

```bash
pwd
sqry index --status .
```
