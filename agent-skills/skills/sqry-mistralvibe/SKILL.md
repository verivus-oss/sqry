---
name: sqry-mistralvibe
version: 33.0.1
description: |
  Setup and workflow for using sqry semantic code search with the Mistral Vibe CLI. CLI-first, with MCP configuration via Vibe's config.toml. Covers installation, tool naming conventions, CLI fallback, and troubleshooting. Tool reference and query syntax are served live by sqry-mcp.
---

# sqry for Mistral Vibe

Use this skill to drive sqry v33.0.1 semantic code search from the Mistral Vibe CLI (`vibe`). The `sqry` CLI works in every Vibe session; MCP is configured through Vibe's TOML config when you want sqry tools available natively.

## Setup

Install or upgrade sqry:

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
sqry --version
sqry-mcp --version
sqry-lsp --version
sqryd --version
```

Install or upgrade Mistral Vibe (separate from sqry; Vibe ships via uv):

```bash
uv tool install --upgrade mistral-vibe
vibe --version
```

Index the project:

```bash
cd /path/to/your/project
sqry index .
sqry index --status --json .
```

## MCP Configuration

Vibe configures MCP servers in TOML, not JSON, and `sqry mcp setup` has no Vibe target. Add a `[[mcp_servers]]` table to `~/.vibe/config.toml` (Vibe uses stdio, http, and streamable-http transports; sqry runs over stdio):

```toml
[[mcp_servers]]
name = "sqry"
transport = "stdio"
command = "/absolute/path/to/sqry-mcp"
args = ["--no-daemon"]
```

For a project-scoped config, set `VIBE_HOME` to a local `.vibe` directory (for example `export VIBE_HOME=/path/to/project/.vibe`) and place `config.toml` there.

API keys and provider credentials live in `~/.vibe/.env`, not `config.toml`. Vibe's CLI does not yet support MCP servers that require OAuth. sqry over stdio is unaffected.

Restart Vibe after editing `config.toml`. Verify with the `/config` command inside Vibe.

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

```toml
args = ["--daemon"]   # only when you accept the 17-tool, no-resource tradeoff
```

## Redaction for external LLMs

Set `SQRY_REDACTION_PRESET` in the process environment before launching `sqry-mcp` (Vibe does not always expose per-server env in TOML; use a wrapper script or shell export if needed). Presets: `none|minimal|relative|standard|strict` (default `minimal`). See `sqry-mcp --help` and the `sqry-mcp-redaction` crate README in the sqry repo. When a `sqryd` daemon is reachable, `sqry-mcp` runs as a shim by default and the daemon redacts under the preset in its own environment, ignoring this one: set it where `sqryd` runs, or pass `--no-daemon` in the server `args`.

## Skill Dependency

Also load `sqry-semantic-search`. It contains the shared routing rules, the table of all 39 tools with their required arguments, CLI fallback commands, ambiguity handling, output-size limits, and rebuild recovery steps.

## Tool Naming

When sqry is configured under the name `sqry`, Vibe exposes its tools under that server name. Read `sqry://meta/manifest` first on **standalone** MCP when resources are available, then use `sqry://docs/capability-map` and `sqry://docs/tool-guide` for the exact installed tool surface. When MCP is unavailable, ignore tool naming and use the CLI.

## CLI Fallback

Vibe can run shell commands via its `bash` tool, so the CLI is a reliable path whenever MCP is not connected. From the workspace root, with flags before the positional query:

```bash
sqry query --json 'kind:function AND name:authenticate'
sqry graph direct-callers --json "AuthService::authenticate"
sqry impact --json "AuthService::authenticate"
sqry cycles --json
sqry diff --json HEAD~1 HEAD
```

There is no natural-language command (`sqry ask` was removed in sqry v21); turn the question into one of the structured commands above.

## Recommended System-Prompt Addition

Vibe routes behaviour through custom system prompts, not an `AGENTS.md`. Create a markdown file in `~/.vibe/prompts/` (for example `sqry.md`) and set `system_prompt_id = "sqry"` in `config.toml`, or append this guidance to your existing custom prompt:

```markdown
## Code Search

For structural code questions (callers, callees, references, imports,
call paths, dependency impact, cycles, unused symbols, semantic diffs)
prefer sqry (CLI or MCP tools when connected) over grep and file-read tools.
Use grep for literal text search and file listing for filename discovery.

On C code, sqry call edges can carry resolution provenance and ambiguous calls
are elided rather than guessed, so an absent edge means "not confidently
resolvable," not "does not exist." For sensitive or regulated repositories,
set `SQRY_REDACTION_PRESET=standard` (or `strict`) before launching sqry-mcp.
```

## Troubleshooting

- Tools not visible: restart Vibe after editing `config.toml`; verify with `/config`.
- Local tools disappeared after adding an MCP server: Vibe's tool allow-list can shadow built-in local tools when MCP is enabled. Re-enable them in your tool permissions (for example with a `*` wildcard) so `bash`, `read`, and `write_file` remain available.
- Name collision: if a same-named MCP binary is also discoverable on `PATH`, Vibe may pick the wrong one. Use an absolute `command` path and a distinct server `name`.
- Empty results: run `sqry index .` from the project root, or `sqry index --force .` after an upgrade or stale graph warning.
- Stale graph or unknown plugin IDs: remove `.sqry/graph`, `.sqry/graphs`, and `.sqry/analysis`, then rebuild.
- Cannot read `sqry://meta/manifest`: switch to standalone `sqry-mcp --no-daemon` (daemon serves zero resources).
- Fewer tools than expected (17 instead of 39): you are on daemon-hosted MCP. Keep `args = ["--no-daemon"]` for the full surface.
- 404 on `sqry://meta/manifest`: old server version; upgrade sqry.
