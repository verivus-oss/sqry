# Using sqry MCP with Codex CLI

**Status**: Supported via `sqry mcp setup`
**Applies to**: Codex CLI using `~/.codex/config.toml`

For Gemini workflow details, see [GEMINI_INTEGRATION.md](GEMINI_INTEGRATION.md).
For shared context/skill definitions across Codex, Claude, and Gemini, see [../docs/LLM_SKILLS_STANDARD.md](../docs/LLM_SKILLS_STANDARD.md).

## Overview

Codex is configured as a global MCP client entry, and sqry now resolves the
workspace per MCP session using this order:

1. Explicit tool `path` when you provide one
2. File-bearing tool arguments such as `file_path` or `expand_files`
3. MCP client roots (`roots/list`) cached for the current session
4. Last resolved workspace for the same session when still valid
5. Legacy env/CWD fallback for clients without roots support

This means:
- `sqry mcp setup --tool codex` writes config once
- The common single-repository session does not require `path` on every tool call
- Explicit `path` is only needed to break ambiguity in multi-root sessions

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
args = ["--no-daemon"]
```

Notes:
- Codex config is global.
- The default setup intentionally avoids pinning `SQRY_MCP_WORKSPACE_ROOT`.
- The default setup passes `--no-daemon` to keep Codex isolated in standalone
  mode. Remove it only if you want the plain `sqry-mcp` daemon probe/fallback
  behavior, or replace it with `--daemon` for forced daemon mode.
- Starting Codex from the target repository directory still works, but it is no
  longer the primary workspace-selection mechanism when Codex exposes MCP roots.

## Manual Configuration

If you need to configure by hand, add the same entry to `~/.codex/config.toml`:

```toml
[mcp_servers.sqry]
command = "/absolute/path/to/sqry-mcp"
args = ["--no-daemon"]
```

Then validate:

```bash
sqry mcp status
```

## Workspace Behavior

Codex works best with session-scoped MCP roots.

Recommended workflow for multiple repositories:

```bash
codex   # open in /repo-a
codex   # open in /repo-b
```

If Codex exposes multiple active roots and a request does not uniquely identify
one workspace, sqry returns a clear error asking for explicit `path`.

## `workspace_status` and Path Arguments

`workspace_status` identifies each source root in
`aggregate.source_root_statuses[]` by an **opaque** `source_root_id`
(8-hex display/correlation token). It is not a path and not a path
prefix.

- Path-taking tools (`get_definition`, `get_document_symbols`,
  `semantic_search` `path`, ...) accept **workspace-relative** paths
  (`src/lib.rs`) or normal filesystem paths.
- Do NOT call tools with `source_root_id`-prefixed paths:
  `485f1995/src/lib.rs` is invalid and will not resolve.
- Under the default `minimal` redaction preset, other tools render
  workspace paths as `<source_root_id>/<relative>`; use the
  `source_root_id` field only to correlate those prefixes with a
  source root's status.
- Earlier releases rendered this token under a per-root `path` field;
  clients reading that field should migrate to `source_root_id`.
- Under `SQRY_REDACTION_PRESET=none`, cleartext source-root locations
  appear through the top-level `source_roots[]` field; per-root entries
  still use `source_root_id`.

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

Check which workspace Codex exposed to the current MCP session. If needed, pass
explicit `path` on the ambiguous request:

```bash
sqry index --status .
```
