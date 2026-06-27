# sqry MCP Server

**Version**: 23.0.0

`sqry-mcp` exposes sqry semantic code-search tools to Model Context Protocol clients.

## Modes

Standalone mode:

```bash
sqry-mcp --no-daemon
```

Daemon-backed mode:

```bash
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Standalone mode currently exposes 36 tools. Daemon-hosted MCP exposes a 15-tool subset backed by `sqryd`. Use dynamic discovery for exact schemas and descriptions:

```bash
sqry-mcp --list-tools
```

MCP clients can also call `tools/list`, and sqry clients can read `sqry://meta/manifest`.

> **Removed in 23.0.0:** the `sqry_ask` natural-language MCP tool was removed. Use `sqry_query`, `semantic_search`, and `relation_query` instead; see [Removed features](../docs/TROUBLESHOOTING.md#removed-features) for migration.

## Setup

```bash
sqry mcp setup --tool claude
sqry mcp setup --tool codex
sqry mcp setup --tool gemini
```

Preview without writing:

```bash
sqry mcp setup --tool codex --dry-run
```

Codex and Gemini use global MCP config and rely on starting the assistant in the target project. Claude project scope can pin a workspace root.

When setup writes a standalone command, expect `--no-daemon`. Use `--daemon` only when you have started and loaded `sqryd` intentionally.

## Tool Discovery

Prefer discovery over copying static tables:

- MCP `tools/list`
- `sqry-mcp --list-tools`
- `sqry://meta/manifest`
- `sqry://docs/tool-guide`
- `sqry://docs/capability-map`

The standalone catalog includes tools for symbol search, navigation, relation queries, graph export, impact analysis, semantic diff, duplicate/cycle/unused detection, workspace status, and context propagation.

## Workspaces

MCP resolves workspace context from explicit paths, file-bearing arguments, MCP roots, recent session context, and current working directory fallback.

For multi-root projects, configure a workspace with `.sqry-workspace` or a VS Code `.code-workspace` file containing `sqry.workspace`.

```bash
sqry workspace status . --json
```

## `source_root_id`

`workspace_status.aggregate.source_root_statuses[].source_root_id` is an opaque 8-hex display/correlation token. It is not a path and not a path prefix.

Invalid tool path:

```text
485f1995/src/lib.rs
```

Valid tool paths:

```text
src/lib.rs
/absolute/path/to/workspace/src/lib.rs
```

Under redaction, result paths can be rendered with a source-root prefix for display. Use `source_root_id` only to correlate that display prefix with aggregate status entries. Cleartext source roots appear only in top-level `source_roots[]` when the selected redaction preset permits them.

## Redaction

The MCP runtime default is `minimal`. For hosted or external LLM providers, `standard` is the recommended preset unless stricter path privacy is required. `strict` is available for stronger redaction.

Keep these separate:

- Runtime default: `minimal`
- Recommended external-provider preset: `standard`
- Library/environment defaults: see `sqry-mcp-redaction` docs for embedding-specific behavior

## Related Guides

- [Public MCP guide](../docs/user-guide/mcp.md)
- [Workspace guide](../docs/user-guide/workspace.md)
- [Daemon guide](../docs/user-guide/daemon.md)
- [Troubleshooting](TROUBLESHOOTING.md)
