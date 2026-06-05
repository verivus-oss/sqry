# sqry MCP User Guide

**Version**: 19.0.7

This guide is the component-level MCP reference. For the public workflow overview, see [docs/user-guide/mcp.md](../docs/user-guide/mcp.md).

## Start

```bash
sqry-mcp --no-daemon
```

Use daemon mode only when `sqryd` is running and the workspace is loaded:

```bash
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

## Discover Tools

Do not rely on copied static catalogs. Discover the current schema:

```bash
sqry-mcp --list-tools
```

MCP clients can call `tools/list`. sqry MCP resources expose `sqry://meta/manifest`, `sqry://docs/tool-guide`, and `sqry://docs/capability-map`.

Standalone `sqry-mcp` currently exposes 37 tools. Daemon-hosted MCP exposes a 16-tool subset.

## Common Tools

Representative standalone tools include:

- `semantic_search`, `hierarchical_search`, `pattern_search`
- `get_definition`, `get_references`, `get_hover_info`, `get_document_symbols`
- `direct_callers`, `direct_callees`, `relation_query`, `trace_path`, `call_hierarchy`
- `dependency_impact`, `show_dependencies`, `subgraph`, `semantic_diff`
- `find_cycles`, `find_duplicates`, `find_unused`, `complexity_metrics`
- `export_graph`, `cross_language_edges`, `context_propagation`
- `workspace_status`, `get_index_status`, `get_graph_stats`
- `sqry_ask`, `sqry_query`

The exact catalog is the live discovery output.

## Workspace Resolution

Prefer explicit path arguments when a client session can see multiple source roots. In a normal single-repo session, MCP can resolve from the current working directory or MCP roots.

Useful CLI checks outside MCP:

```bash
sqry workspace status . --json
sqry index --status --json .
```

## `workspace_status`

`workspace_status` returns aggregate workspace health and identity projections. The key safety rule:

`source_root_id` is an opaque 8-hex display/correlation token. It is not a filesystem path and not a valid path prefix.

Do not pass:

```json
{ "path": "485f1995/src/lib.rs" }
```

Pass:

```json
{ "path": "src/lib.rs" }
```

or an ordinary absolute path.

Legacy clients that previously read path-shaped aggregate fields should migrate to `source_root_id` for display/correlation only. Cleartext source-root paths are carried separately in top-level `source_roots[]` when redaction allows them.

## Redaction Presets

- `minimal`: MCP runtime default.
- `standard`: recommended for external/hosted LLM providers.
- `strict`: stronger path privacy with more opaque output.

Preset behavior affects display paths and cleartext carriers, not the rule for tool input paths.

## Natural Language

`sqry_ask` translates a request into a validated sqry command.

```json
{
  "query": "who calls authenticate",
  "path": ".",
  "execute": false
}
```

Set `execute` deliberately. Model downloads and unverified model loading are opt-in.

## Daemon Mode Notes

Daemon mode is useful for repeated assistant sessions and editor workflows where a warm graph is more important than the full standalone tool surface.

If daemon and disk state disagree:

```bash
sqry daemon status --json
sqry daemon reset .
sqry index --force .
sqry daemon load .
```

## More Detail

- [MCP public guide](../docs/user-guide/mcp.md)
- [Workspace guide](../docs/user-guide/workspace.md)
- [Daemon guide](../docs/user-guide/daemon.md)
- [Troubleshooting](TROUBLESHOOTING.md)
