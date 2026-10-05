---
name: sqry-claude
version: 33.0.0
description: |
  Setup and workflow for using sqry semantic code search as an MCP server with Claude Code. Covers installation, MCP configuration, tool naming conventions, CLI fallback, and troubleshooting. Tool reference and query syntax are served live by sqry-mcp.
---

# sqry for Claude Code

Use this skill to configure Claude Code for sqry v33.0.0 MCP-backed semantic code search.

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

Configure Claude Code:

```bash
sqry mcp setup --tool claude
sqry mcp status
```

By default (`--scope auto`) this writes a project-scoped entry into `~/.claude.json` under `projects["<project root>"].mcpServers.sqry`, with `"type": "stdio"`, `"args": ["--no-daemon"]` and `SQRY_MCP_WORKSPACE_ROOT` pinned to the project root. `--scope global` writes a global entry instead (workspace resolved from the Claude Code launch directory); `--workspace-root <path>` overrides the pinned root; `--dry-run` prints the entry without writing.
Restart Claude Code after setup so it reloads MCP servers.

Manual config (same shape `sqry mcp setup` writes):

```json
{
  "mcpServers": {
    "sqry": {
      "type": "stdio",
      "command": "/absolute/path/to/sqry-mcp",
      "args": ["--no-daemon"],
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      }
    }
  }
}
```

Omit `SQRY_MCP_WORKSPACE_ROOT` when the same entry serves several repos or git worktrees; sqry-mcp then resolves the workspace from explicit `path` arguments, MCP roots (Claude Code answers with the launch directory), the last-resolved workspace, then the CWD.

### MCP mode: standalone vs daemon

**Default:** standalone `sqry-mcp --no-daemon`. Serves **39 tools**, **6 MCP resources** (`sqry://meta/manifest` and `sqry://docs/*`) and **6 prompts**.

**Daemon** (`sqry-mcp --daemon`) warms the graph for long sessions but exposes only a **17-tool subset**, **zero MCP resources** and **zero prompts**: agents cannot read `sqry://meta/manifest` or docs on the daemon path. Do not configure daemon then instruct reading MCP resources in the same workflow. With no flag at all, `sqry-mcp` probes for a running `sqryd` and connects to it when reachable, so an unflagged entry can land on the 17-tool subset silently.

```bash
# Standalone: full tools + resources + prompts (preferred)
sqry-mcp --no-daemon

# Daemon: warm graph, 17-tool subset, no resources
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Add `"args": ["--daemon"]` only when you accept the 17-tool, no-resource tradeoff.

## Skill Dependency

Also load `sqry-semantic-search`. It contains the shared routing rules, the table of all 39 tools with their required arguments, CLI fallback commands, ambiguity handling, output-size limits, and rebuild recovery steps.

## Tool Naming

Claude Code exposes sqry MCP tools with the `mcp__sqry__` prefix, for example `mcp__sqry__semantic_search`, `mcp__sqry__direct_callers` and `mcp__sqry__get_graph_stats` (`get_graph_stats` is standalone-only). The 6 MCP prompts appear as slash commands: `/mcp__sqry__semantic_search`, `/mcp__sqry__find_callers`, `/mcp__sqry__find_callees`, `/mcp__sqry__trace_path`, `/mcp__sqry__explain_symbol`, `/mcp__sqry__code_impact`.

Read `sqry://meta/manifest` first when resources are available, then use `sqry://docs/capability-map` and `sqry://docs/tool-guide` for the exact installed tool surface.

## CLI Fallback

If Claude Code cannot see sqry MCP tools after setup or before restart, use (flags before the positional query):

```bash
sqry query --json 'kind:function AND name:authenticate'
sqry graph direct-callers --json "AuthService::authenticate"
sqry impact --json "AuthService::authenticate"
```

## Recommended CLAUDE.md Addition

```markdown
## Code Search

Use sqry MCP tools (`mcp__sqry__*`) for semantic code search.
Read `sqry://docs/capability-map` to find the right tool.
Use `sqry` CLI as fallback when MCP is unavailable.
Use Grep for literal text search and Glob for file finding.
```

## Troubleshooting

- No tools visible: restart Claude Code after `sqry mcp setup --tool claude`; `claude mcp list` shows what is registered.
- Fewer than 39 tools: the entry has no `--no-daemon` and a `sqryd` is running, so you are on the 17-tool daemon subset.
- Empty results: run `sqry index .` from the project root, or `sqry index --force .` after an upgrade or stale graph warning.
- Stale graph or unknown plugin IDs: remove `.sqry/graph`, `.sqry/graphs`, and `.sqry/analysis`, then rebuild.
- Transport error on resource read: MCP server is not running or not configured.
- 404 on `sqry://meta/manifest`: old server version; upgrade sqry, or switch to standalone `sqry-mcp --no-daemon` (daemon serves zero resources).
