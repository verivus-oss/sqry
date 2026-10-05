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

Standalone `sqry-mcp` currently exposes 39 tools. Daemon-hosted MCP exposes a 17-tool subset for daemon-backed workflows. Prefer dynamic discovery for exact schemas:

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

The MCP runtime default is `minimal`. Presets are `none`, `minimal`, `relative` (legible workspace-relative paths), `standard`, and `strict`. For external or hosted LLM providers, `standard` is the recommended preset unless you need stricter path privacy. `strict` hides more path detail and can require more correlation work from the client. Override with `SQRY_REDACTION_PRESET`. The name is read trimmed and in any letter case (`Strict` is `strict`). Any other value is refused at startup with an error naming it, rather than served unredacted, by `sqryd` and by a standalone `sqry-mcp` (in-process mode, `--no-daemon`, or the fallback when no daemon is reachable). A `sqry-mcp` running as a shim does not read the variable, so it neither applies nor refuses it; see below.

Where that variable must be set depends on which process serves the tools. A standalone `sqry-mcp` (in-process mode, or `--no-daemon`) reads it from its own environment, the `env` block of your MCP client configuration. When a `sqryd` daemon is reachable, `sqry-mcp` runs as a shim by default and the daemon serves and redacts every response under the preset in **its own** environment; the client's `SQRY_REDACTION_PRESET` does not reach it and is ignored. To get a stricter preset while a daemon runs, either set `SQRY_REDACTION_PRESET` where `sqryd` runs (its service unit, or the shell that starts it, then restart it) or start `sqry-mcp --no-daemon` with the variable in its `env`.

## More Detail

- [sqry-mcp README](../../sqry-mcp/README.md)
- [sqry-mcp User Guide](../../sqry-mcp/USER_GUIDE.md)
- [sqry-mcp Troubleshooting](../../sqry-mcp/TROUBLESHOOTING.md)
- [Workspaces](workspace.md)
- [Daemon Mode](daemon.md)
