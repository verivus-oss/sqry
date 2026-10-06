---
name: sqry-opencode
version: 33.0.1
description: |
  Setup and workflow for using sqry semantic code search as an MCP server with OpenCode. Covers installation, MCP configuration, tool naming conventions, CLI fallback, and troubleshooting. Tool reference and query syntax are served live by sqry-mcp.
---

# sqry for OpenCode

Use this skill to configure OpenCode for sqry v33.0.1 MCP-backed semantic code search.

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

OpenCode configures MCP servers in `opencode.json` or `opencode.jsonc` under the `mcp` key, globally at `~/.config/opencode/opencode.json` or per project in the repo root (`sqry mcp setup` has no OpenCode target; write the entry by hand). Local (stdio) servers use a `command` array:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "sqry": {
      "type": "local",
      "command": ["/absolute/path/to/sqry-mcp", "--no-daemon"],
      "environment": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      },
      "enabled": true
    }
  }
}
```

Local server options: `type` (`"local"`, required), `command` (array, required), `environment` (object), `enabled` (boolean), `timeout` (ms for the tools handshake; default 5000). If sqry's first index or tool listing is slow on a large repo, raise `timeout`. Omit `SQRY_MCP_WORKSPACE_ROOT` in the global config when one entry serves several repos; sqry-mcp then resolves the workspace from explicit `path` arguments, MCP roots, the last-resolved workspace, then the CWD.

Restart OpenCode after editing the config so it reloads MCP servers.

OpenCode also supports Agent Skills natively (see https://opencode.ai/docs/skills/), so this skill set can be installed into OpenCode directly as well as configured as an MCP server.

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

Put `"--daemon"` in the `command` array only when you accept the 17-tool, no-resource tradeoff.

## Redaction for external LLMs

Set `SQRY_REDACTION_PRESET` in the server `environment` before exposing graph results to cloud or untrusted models (`none|minimal|relative|standard|strict`; default `minimal`). See `sqry-mcp --help` and the `sqry-mcp-redaction` crate README in the sqry repo. `relative` rewrites absolute paths to workspace-relative ones; `standard` for cloud LLMs when code must stay confidential; `strict` for untrusted externals. When a `sqryd` daemon is reachable, `sqry-mcp` runs as a shim by default and the daemon redacts under the preset in its own environment, ignoring this one: set it where `sqryd` runs, or pass `--no-daemon` in the server `args`.

## Skill Dependency

Also load `sqry-semantic-search`. It contains the shared routing rules, the table of all 39 tools with their required arguments, CLI fallback commands, ambiguity handling, output-size limits, and rebuild recovery steps.

## Tool Naming

OpenCode registers MCP tools with the server name as a prefix, so under the server name `sqry` the tools are exposed as `sqry_*` (for example `sqry_semantic_search`). This is deterministic, not version-dependent. Read `sqry://meta/manifest` first on **standalone** MCP when resources are available, then use `sqry://docs/capability-map` and `sqry://docs/tool-guide` for the exact installed tool surface.

## CLI Fallback

If OpenCode cannot see sqry MCP tools after setup or before restart, use the CLI from the workspace root (flags before the positional query):

```bash
sqry query --json 'kind:function AND name:authenticate'
sqry graph direct-callers --json "AuthService::authenticate"
sqry impact --json "AuthService::authenticate"
```

OpenCode can run shell commands, so the CLI path is a reliable recovery route whenever MCP is not connected.

## Recommended AGENTS.md Addition

OpenCode reads project instructions from `AGENTS.md` in the project root. Add:

```markdown
## Code Search

For structural code questions (callers, callees, references, imports,
call paths, dependency impact, cycles, unused symbols, semantic diffs)
prefer sqry over the built-in grep and file-read tools.

Use sqry MCP tools (`sqry_*`) when connected; read `sqry://docs/capability-map`
to find the right tool. Use the `sqry` CLI as fallback when MCP is unavailable.
Use grep for literal text search and file listing for filename discovery.

On C code, sqry call edges can carry resolution provenance and ambiguous calls
are elided rather than guessed, so an absent edge means "not confidently
resolvable," not "does not exist." For sensitive or regulated repositories,
set `SQRY_REDACTION_PRESET=standard` (or `strict`) in the sqry MCP server
environment before exposing graph results to an external model.
```

## Troubleshooting

- No tools visible: restart OpenCode after editing `opencode.json`. List configured servers with `opencode mcp list`.
- Empty results: run `sqry index .` from the project root, or `sqry index --force .` after an upgrade or stale graph warning.
- Stale graph or unknown plugin IDs: remove `.sqry/graph`, `.sqry/graphs`, and `.sqry/analysis`, then rebuild.
- Tools handshake times out on a large repo: raise the server `timeout` (default 5000 ms) in the config.
- Cannot read `sqry://meta/manifest`: switch to standalone `sqry-mcp --no-daemon` (daemon serves zero resources).
- Fewer tools than expected (17 instead of 39): you are on daemon-hosted MCP. Keep `--no-daemon` in the `command` array for the full surface.
- Transport error on resource read: MCP server is not running or not configured.
- 404 on `sqry://meta/manifest`: old server version; upgrade sqry.
