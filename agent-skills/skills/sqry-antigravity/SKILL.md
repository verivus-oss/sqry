---
name: sqry-antigravity
version: 33.0.1
description: |
  Setup and workflow for using sqry semantic code search as an MCP server with Google Antigravity (IDE and CLI). Covers installation, MCP configuration, tool naming conventions, CLI fallback, and troubleshooting. Tool reference and query syntax are served live by sqry-mcp.
---

# sqry for Antigravity

Use this skill to configure Google Antigravity (the IDE and the `agy` CLI) for sqry v33.0.1 MCP-backed semantic code search.

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

Open Antigravity's MCP config. In the IDE: click the "..." menu in the Agent panel, choose MCP Servers, then Manage MCP Servers, then View raw config; this opens `mcp_config.json`. (Alternatively, Settings, then Customizations, then Open MCP Config.) The file is often at `~/.gemini/config/mcp_config.json`, which Antigravity shares with the Gemini CLI; some IDE installs use `~/.gemini/antigravity/mcp_config.json`. Use "View raw config" to open whichever file is active. `sqry mcp setup` has no Antigravity target; write the entry by hand.

Antigravity uses a single `mcpServers` object. A local (stdio) server takes a string `command` plus an `args` array and an `env` object:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry-mcp",
      "args": ["--no-daemon"],
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      }
    }
  }
}
```

(For remote HTTP servers Antigravity uses `serverUrl`, not `url`; not needed for sqry's local stdio server.)

Save the file; Antigravity reloads MCP configuration automatically. If it does not pick up the change, restart Antigravity.

**Config collision:** Antigravity and Gemini CLI may share `~/.gemini/config/mcp_config.json`. If you also use `sqry-gemini`, both hosts read the same sqry entry. That is usually fine, but verify workspace roots do not pin the wrong repo; omit `SQRY_MCP_WORKSPACE_ROOT` when one entry serves several repos.

Antigravity also supports Agent Skills across its IDE and CLI, so this skill set can be installed into Antigravity directly as well as configured as an MCP server.

### MCP mode: standalone vs daemon

**Default:** standalone `sqry-mcp --no-daemon`. Serves **39 tools**, **6 MCP resources** (`sqry://meta/manifest` and `sqry://docs/*`) and **6 prompts**.

**Daemon** (`sqry-mcp --daemon`) warms the graph for long sessions but exposes only a **17-tool subset**, **zero MCP resources** and **zero prompts**: agents cannot read `sqry://meta/manifest` or docs on the daemon path. Do not configure daemon then instruct reading MCP resources in the same workflow. With no flag at all, `sqry-mcp` probes for a running `sqryd` and connects to it when reachable.

```bash
# Standalone: full tools + resources + prompts (preferred)
sqry-mcp --no-daemon

# Daemon: warm graph, 17-tool subset, no resources
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Set `"args": ["--daemon"]` only when you accept the 17-tool, no-resource tradeoff.

## Redaction for external LLMs

Set `SQRY_REDACTION_PRESET` in the server `env` before exposing graph results to cloud or untrusted models (`none|minimal|relative|standard|strict`; default `minimal`). See `sqry-mcp --help` and the `sqry-mcp-redaction` crate README in the sqry repo. When a `sqryd` daemon is reachable, `sqry-mcp` runs as a shim by default and the daemon redacts under the preset in its own environment, ignoring this one: set it where `sqryd` runs, or pass `--no-daemon` in the server `args`.

## Skill Dependency

Also load `sqry-semantic-search`. It contains the shared routing rules, the table of all 39 tools with their required arguments, CLI fallback commands, ambiguity handling, output-size limits, and rebuild recovery steps.

## Tool Naming

Antigravity shares Gemini's MCP host conventions and commonly exposes sqry tools with a server prefix (for example `sqry__semantic_search` or canonical `semantic_search`, depending on version). Read `sqry://meta/manifest` first on **standalone** MCP when resources are available, then use `sqry://docs/capability-map` and `sqry://docs/tool-guide` for the exact installed tool surface.

## CLI Fallback

If Antigravity cannot see sqry MCP tools after setup or before restart, use the CLI from the workspace root (flags before the positional query):

```bash
sqry query --json 'kind:function AND name:authenticate'
sqry graph direct-callers --json "AuthService::authenticate"
sqry impact --json "AuthService::authenticate"
```

## Recommended AGENTS.md Addition

Antigravity reads project instructions from `AGENTS.md` (project root, or the `antigravity-cli/AGENTS.md` location used by some installs). Add:

```markdown
## Code Search

For structural code questions (callers, callees, references, imports,
call paths, dependency impact, cycles, unused symbols, semantic diffs)
prefer sqry over the built-in grep and file-read tools.

Use sqry MCP tools when connected; read `sqry://docs/capability-map` to find
the right tool. Use the `sqry` CLI as fallback when MCP is unavailable. Use
grep for literal text search and file listing for filename discovery.

On C code, sqry call edges can carry resolution provenance and ambiguous calls
are elided rather than guessed, so an absent edge means "not confidently
resolvable," not "does not exist." For sensitive or regulated repositories,
set `SQRY_REDACTION_PRESET=standard` (or `strict`) in the sqry MCP server
env before exposing graph results to an external model.
```

## Troubleshooting

- No tools visible: save `mcp_config.json` and use Refresh under Customizations, Installed MCP Servers; restart Antigravity if needed.
- Verify connection: in the Antigravity CLI open the `/mcp` panel; in the IDE check Customizations, Installed MCP Servers and select sqry to list its tools.
- Config not picked up: confirm whether your install reads `~/.gemini/config/mcp_config.json` or `~/.gemini/antigravity/mcp_config.json`; "View raw config" always opens the active file.
- Empty results: run `sqry index .` from the project root, or `sqry index --force .` after an upgrade or stale graph warning.
- Stale graph or unknown plugin IDs: remove `.sqry/graph`, `.sqry/graphs`, and `.sqry/analysis`, then rebuild.
- Cannot read `sqry://meta/manifest`: switch to standalone `sqry-mcp --no-daemon` (daemon serves zero resources).
- Fewer tools than expected (17 instead of 39): you are on daemon-hosted MCP. Keep `"--no-daemon"` in `args` for the full surface.
- 404 on `sqry://meta/manifest`: old server version; upgrade sqry.
