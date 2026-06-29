# MCP Guide

sqry ships `sqry-mcp` so AI assistants can call semantic code-search tools over the Model Context Protocol.

## Setup

```bash
sqry mcp setup --tool claude
sqry mcp setup --tool codex
sqry mcp setup --tool gemini
```

Use `--dry-run` to preview config changes:

```bash
sqry mcp setup --tool codex --dry-run
```

Codex and Gemini use global MCP configuration and rely on launching the assistant from the target project. Claude project scope can pin a workspace root.

## Standalone Versus Daemon

Standalone:

```bash
sqry-mcp --no-daemon
```

Daemon-backed:

```bash
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Standalone `sqry-mcp` currently exposes 37 tools. Daemon-hosted MCP exposes a 16-tool subset for daemon-backed workflows. Prefer dynamic discovery for exact schemas:

```bash
sqry-mcp --list-tools
```

MCP clients can also use `tools/list`, and sqry clients can read `sqry://meta/manifest`.

## Query Predicates And Structured Parameters

Search tools keep three surfaces separate:

- `query`: string predicates such as `lang:rust`, `kind:function`, `items:true`, and `is_definition:true`.
- `filters`: a structured JSON object for simple pre-filtering, such as `{ "language": ["rust"], "visibility": "public" }`.
- tool-specific structured parameters, such as `list_symbols.items_only=true`.

For definition-only behavior, including the pre-V16 `reindexRequired` advisory, see [Advanced Analysis](advanced-analysis.md#definition-only-queries).

## Source Root IDs

`workspace_status.aggregate.source_root_statuses[].source_root_id` is an opaque 8-hex display/correlation token. It is not a filesystem path and not a path prefix.

Do not call tools with paths like:

```text
485f1995/src/lib.rs
```

Use normal paths instead:

```text
src/lib.rs
/absolute/path/to/repo/src/lib.rs
```

Cleartext source-root paths appear only through top-level `source_roots[]` when the redaction preset permits it.

## Redaction

The MCP runtime default is `minimal`. For external or hosted LLM providers, `standard` is the recommended preset unless you need stricter path privacy. `strict` hides more path detail and can require more correlation work from the client.

## More Detail

- [sqry-mcp README](../../sqry-mcp/README.md)
- [sqry-mcp User Guide](../../sqry-mcp/USER_GUIDE.md)
- [sqry-mcp Troubleshooting](../../sqry-mcp/TROUBLESHOOTING.md)
- [Workspaces](workspace.md)
- [Daemon Mode](daemon.md)
