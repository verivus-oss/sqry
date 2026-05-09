# Using sqry MCP with Claude Code

**Status**: Fully Supported via `sqry mcp setup`
**Applies to**: Claude Code using `.claude.json` (per-project) or `~/.claude.json` (global)

For Codex CLI workflow details, see [CODEX_INTEGRATION.md](CODEX_INTEGRATION.md). For Gemini CLI workflow details, see [GEMINI_INTEGRATION.md](GEMINI_INTEGRATION.md).
For shared context/skill definitions across Codex, Claude, and Gemini, see [../docs/LLM_SKILLS_STANDARD.md](../docs/LLM_SKILLS_STANDARD.md).

## Overview

Claude Code natively supports MCP servers. sqry is configured as a stdio MCP server entry, giving Claude Code direct access to all 33+ sqry tools for semantic code search.

For sqry's auto-setup:
- `sqry mcp setup --tool claude` writes per-project config with pinned `SQRY_MCP_WORKSPACE_ROOT`
- The generated entry passes `--no-daemon` so each Claude Code project stays in
  isolated standalone mode unless you intentionally opt into daemon mode
- Each project gets its own isolated workspace binding
- Launch Claude Code from the repository you want to analyze

## Quick Start

### Auto-Setup (Recommended)

```bash
cd /path/to/your/project
sqry index .
sqry mcp setup --tool claude
```

This writes a per-project entry under `projects[canonical_path].mcpServers.sqry` with an explicit `SQRY_MCP_WORKSPACE_ROOT` environment variable pinned to the project directory.

### Manual Setup

Add to `.claude.json` in your project root (or `~/.claude.json` for global):

```json
{
  "mcpServers": {
    "sqry": {
      "type": "stdio",
      "command": "sqry-mcp",
      "args": ["--no-daemon"],
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      }
    }
  }
}
```

## Available Tools

Once configured, Claude Code can use all sqry MCP tools directly:

- **Search**: `semantic_search`, `pattern_search`, `get_workspace_symbols`, `hierarchical_search`
- **Navigate**: `get_definition`, `get_references`, `get_hover_info`, `get_document_symbols`
- **Relationships**: `relation_query`, `direct_callers`, `direct_callees`, `call_hierarchy`, `trace_path`
- **Analysis**: `dependency_impact`, `show_dependencies`, `subgraph`, `find_cycles`, `find_unused`, `find_duplicates`, `complexity_metrics`
- **Diff**: `semantic_diff`
- **Index**: `get_index_status`, `get_graph_stats`, `get_insights`, `list_files`, `list_symbols`, `rebuild_index`
- **Cross-language**: `cross_language_edges`
- **Natural language**: `sqry_ask`
- **Export**: `export_graph`
- **Similar**: `search_similar`
- **Explain**: `explain_code`

## Per-Project vs Global Config

| Scope | Config Location | When to Use |
|-------|----------------|-------------|
| Per-project (default) | `.claude.json` in repo root | Recommended — isolates workspace root per project |
| Global | `~/.claude.json` | When you want sqry available everywhere (uses CWD discovery) |

## Troubleshooting

### Issue: "sqry-mcp not found"

Ensure `sqry-mcp` is on your PATH:

```bash
which sqry-mcp
# If not found, add to PATH or use absolute path in config
```

### Issue: Stale search results

Rebuild the index:

```bash
sqry index --force .
```

### Issue: MCP server not connecting

Check that the config is valid JSON and restart Claude Code.

## See Also

- User Guide: [USER_GUIDE.md](USER_GUIDE.md)
- Codex Integration: [CODEX_INTEGRATION.md](CODEX_INTEGRATION.md)
- Gemini Integration: [GEMINI_INTEGRATION.md](GEMINI_INTEGRATION.md)
