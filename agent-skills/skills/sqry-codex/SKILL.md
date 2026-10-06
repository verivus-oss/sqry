---
name: sqry-codex
version: 33.0.1
description: |
  Setup and workflow for using sqry semantic code search as an MCP server with OpenAI Codex CLI. Covers installation, MCP configuration, CLI fallback, and troubleshooting. Tool reference and query syntax are served live by sqry-mcp.
---

# sqry for OpenAI Codex

Use this skill to configure Codex CLI for sqry v33.0.1 MCP-backed semantic code search.

## Setup

Install or upgrade sqry:

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
sqry --version
sqry-mcp --version
sqry-lsp --version
sqryd --version
```

Index the project:

```bash
cd /path/to/your/project
sqry index .
sqry index --status --json .
```

Configure Codex:

```bash
sqry mcp setup --tool codex
sqry mcp status
```

Restart Codex after setup so it reloads MCP servers.

This writes a global entry to `~/.codex/config.toml` (the exact shape `--dry-run` prints):

```toml
[mcp_servers.sqry]
command = "/absolute/path/to/sqry-mcp"
args = ["--no-daemon"]
```

Codex uses global MCP config, so `sqry mcp setup` rejects `--workspace-root` for it. sqry-mcp resolves workspaces session-scoped: explicit `path` arguments first, then file-bearing arguments, MCP roots, last-resolved workspace, and legacy environment/CWD fallback. Start Codex from the project directory for the simplest single-repo flow.

### MCP mode: standalone vs daemon

**Default:** standalone `sqry-mcp --no-daemon`. Serves **39 tools**, **6 MCP resources** (`sqry://meta/manifest` and `sqry://docs/*`) and **6 prompts**.

**Daemon** (`sqry-mcp --daemon`) warms the graph for long sessions but exposes only a **17-tool subset**, **zero MCP resources** and **zero prompts**: agents cannot read `sqry://meta/manifest` or docs on the daemon path. With no flag at all, `sqry-mcp` probes for a running `sqryd` and connects to it when reachable.

```bash
# Standalone: full tools + resources + prompts (preferred)
sqry-mcp --no-daemon

# Daemon: warm graph, 17-tool subset, no resources
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Set `args = ["--daemon"]` under `[mcp_servers.sqry]` only when you accept the 17-tool, no-resource tradeoff.

## Skill Dependency

Also load `sqry-semantic-search`. It contains the shared routing rules, the table of all 39 tools with their required arguments, CLI fallback commands, ambiguity handling, output-size limits, and rebuild recovery steps.

## Tool Naming

Codex commonly exposes sqry MCP tools with the `mcp__sqry__` prefix, for example `mcp__sqry__semantic_search` and `mcp__sqry__get_graph_stats` (`get_graph_stats` is standalone-only).

Read `sqry://meta/manifest` first when resources are available, then use `sqry://docs/capability-map` and `sqry://docs/tool-guide` for the exact installed tool surface.

## CLI Fallback

If Codex cannot see sqry MCP tools after setup or before restart, use (flags before the positional query):

```bash
sqry query --json 'kind:function AND name:authenticate'
sqry graph direct-callers --json "AuthService::authenticate"
sqry impact --json "AuthService::authenticate"
```

## Recommended AGENTS.md Addition

```markdown
## Code Search

Use sqry MCP tools for semantic code search.
Read `sqry://docs/capability-map` to find the right tool.
Use `sqry` CLI as fallback when MCP is unavailable.
Use `rg` for literal text search.
```

## Troubleshooting

- No tools visible: restart Codex after `sqry mcp setup --tool codex`.
- Fewer than 39 tools: the entry has no `--no-daemon` and a `sqryd` is running, so you are on the 17-tool daemon subset.
- Empty results: run `sqry index .` from the project root, or `sqry index --force .` after an upgrade or stale graph warning.
- Stale graph or unknown plugin IDs: remove `.sqry/graph`, `.sqry/graphs`, and `.sqry/analysis`, then rebuild.
- Transport error on resource read: MCP server is not running or not configured.
- 404 on `sqry://meta/manifest`: old server version; upgrade sqry, or switch to standalone `sqry-mcp --no-daemon` (daemon serves zero resources).
