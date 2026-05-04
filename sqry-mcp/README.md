# sqry MCP Server - by Verivus

**Connect your AI assistant to sqry's semantic code search**

Integrate sqry with Codex, Claude Desktop, Windsurf, Cursor, and other AI coding assistants via the Model Context Protocol (MCP).

---

## What You Can Do

With sqry MCP, your AI assistant can:

- 🔍 **Search** your codebase semantically (fuzzy, typo-tolerant)
- 🎯 **Query** with structured JSON filters (language, kind, visibility, score) — separate from query string predicates
- 🔗 **Navigate** relationships (callers, callees, imports, exports)
- 📊 **Analyze** dependencies and impact
- 💡 **Explain** code symbols with context
- 🔄 **Compare** semantic changes between git commits

**Example prompts**:
```
@sqry find all authentication functions
@sqry who calls the processUser function?
@sqry analyze impact of changing the User class
@sqry what changed between main and feature-branch?
```

---

## Quick Start

### 1. Install sqry CLI and MCP Server

```bash
cargo install --path sqry-cli
cargo install --path sqry-mcp
sqry --version  # Verify installation
```

### 2. Index Your Project

```bash
cd /path/to/your/project
sqry index
```

For repeated AI-assistant calls, keep the graph warm in `sqryd` and run MCP as
a daemon shim:

```bash
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

If `sqry-mcp --daemon` cannot reach a daemon, it auto-starts one unless
`SQRY_DAEMON_NO_AUTO_START=1` is set. Use `sqry daemon rebuild <path>` to
refresh a loaded workspace without restarting the assistant session.

### 3. Auto-Configure AI Tools

```bash
# Auto-detect and configure Claude Code, Codex, Gemini CLI
sqry mcp setup

# Check configuration status
sqry mcp status
```

### Codex CLI Notes

`sqry mcp setup --tool codex` writes a global Codex entry to `~/.codex/config.toml`:

```toml
[mcp_servers.sqry]
command = "/absolute/path/to/sqry-mcp"
```

Codex now uses session-scoped workspace resolution in `sqry-mcp`: explicit
`path` first, then file-bearing args, then MCP roots cached for the current
session, then last-resolved workspace, then legacy env/CWD fallback. In the
common single-repository session you do not need `path` on every call.
For ambiguous multi-root sessions, pass `path` or a file-bearing argument so
the tool resolves the intended source root.

### Gemini CLI Notes

`sqry mcp setup --tool gemini` writes a Gemini entry to `~/.gemini/settings.json`:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

Gemini can use user-level and project-level settings files. `sqry-mcp` uses the
same session-scoped workspace resolution flow as Codex, so explicit `path` is
mainly needed only for ambiguous multi-root sessions.

### Manual Configuration

For AI tools not supported by `sqry mcp setup` (Claude Desktop, Windsurf, Cursor):

**Claude Desktop** (`~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      }
    }
  }
}
```

**Other platforms**:
- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Linux**: `~/.config/Claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

**Windsurf / Cursor**: See [User Guide](USER_GUIDE.md#getting-started)

Manual per-project `SQRY_MCP_WORKSPACE_ROOT` pinning still works, but it is now
legacy behavior for clients that do not expose MCP roots.

### 4. Index Your Project

```bash
cd /your/project
sqry index .
```

### 5. Start Using!

```
@sqry find all async functions
@sqry explain the authenticate function in src/auth.ts
```

### MCP Tool Call Example

The `semantic_search` tool accepts a `query` string (with predicates) and an
optional `filters` JSON object:

```json
{
  "method": "tools/call",
  "params": {
    "name": "semantic_search",
    "arguments": {
      "query": "kind:function name~=/^auth/",
      "filters": {
        "language": ["rust", "typescript"],
        "visibility": "public"
      },
      "max_results": 50
    }
  }
}
```

> **Note:** The `filters` parameter is a JSON object, not a string. For
> string-style predicates like `lang:rust`, use the `query` parameter.

---

## Context Files vs Skills

For shared terminology and standards across Codex, Claude, and Gemini, see [../docs/LLM_SKILLS_STANDARD.md](../docs/LLM_SKILLS_STANDARD.md).

Quick model:
- **Persistent context files** define repository policy and workflow (`AGENTS.md`, `CODEX.md`, `CLAUDE.md`, `GEMINI.md`).
- **Skills** are on-demand capability packs using `SKILL.md` (`.claude/skills`, `.gemini/skills`, repo-local `skills/` wrappers).
- **`agents/openai.yaml`** is optional Codex/OpenAI UI metadata, not a cross-vendor standard.

---

## Documentation

- **[User Guide](USER_GUIDE.md)** - Complete installation, setup, and usage guide
- **[MCP Redaction Guide](../sqry-mcp-redaction/README.md)** - Protect paths/code/docs when using external LLM services
- **[Codex Integration](CODEX_INTEGRATION.md)** - Codex CLI setup, verification, and troubleshooting
- **[Gemini Integration](GEMINI_INTEGRATION.md)** - Gemini CLI setup, verification, and troubleshooting
- **[LLM Skills Standard](../docs/LLM_SKILLS_STANDARD.md)** - Shared context/skill definitions and best practices
- **[Troubleshooting](TROUBLESHOOTING.md)** - Common issues and solutions
- **Integration Guides**:
  - [Claude Code Workflow](CLAUDE_CODE_INTEGRATION.md)
  - [Claude Desktop Setup](USER_GUIDE.md#claude-desktop-setup)
  - [Windsurf Setup](USER_GUIDE.md#windsurf-setup)
  - [Cursor Setup](USER_GUIDE.md#cursor-setup)

---

## Response Redaction (Recommended for External LLMs)

When MCP responses leave your local machine (for hosted cloud models), redact
paths and code context before transmission.

- Library: [sqry-mcp-redaction](../sqry-mcp-redaction/README.md)
- Runtime default: `minimal` (via `SQRY_REDACTION_PRESET`, see config table above)
- Presets: `none`, `minimal` (runtime default), `standard` (recommended for external LLMs), `strict`
- Coverage: absolute paths, file URIs, workspace roots, optional code/docs fields

Use `standard` or `strict` when sending responses to external/hosted LLM providers.

---

## Features

### Tool Catalog (34 tools)

| Category | Tool | Purpose |
|----------|------|---------|
| Search | `semantic_search` | Fuzzy symbol search with filters |
| Search | `hierarchical_search` | RAG-optimized grouped search |
| Search | `pattern_search` | Substring match on symbol names |
| Relations | `relation_query` | Callers/callees/imports/exports/returns |
| Relations | `direct_callers` | Direct callers (depth=1) |
| Relations | `direct_callees` | Direct callees (depth=1) |
| Relations | `call_hierarchy` | Call hierarchy tree (incoming/outgoing) |
| Explain | `explain_code` | Symbol details with context |
| Similarity | `search_similar` | Find similar symbols |
| Dependencies | `show_dependencies` | Dependency tree for a file/symbol |
| Graph | `export_graph` | Export DOT/D2/Mermaid/JSON graphs |
| Graph | `cross_language_edges` | Cross-language relationships |
| Graph | `trace_path` | Call-path tracing |
| Graph | `subgraph` | Focused subgraph extraction |
| Analysis | `find_cycles` | Call/import/module cycles |
| Analysis | `is_node_in_cycle` | Check if a symbol is in a cycle |
| Analysis | `find_duplicates` | Duplicate code detection |
| Analysis | `find_unused` | Dead code detection |
| Analysis | `dependency_impact` | Reverse dependency impact |
| Analysis | `semantic_diff` | Semantic git diff |
| Index | `get_index_status` | Index status/metadata |
| Index | `rebuild_index` | Rebuild unified graph index |
| Introspection | `list_files` | List indexed files |
| Introspection | `list_symbols` | List indexed symbols |
| Introspection | `get_graph_stats` | Graph statistics |
| Introspection | `get_insights` | Codebase health indicators |
| Introspection | `complexity_metrics` | Complexity metrics |
| Navigation | `get_definition` | Definition lookup |
| Navigation | `get_references` | Reference lookup |
| Navigation | `get_hover_info` | Hover info |
| Navigation | `get_document_symbols` | Symbols in a file |
| Navigation | `get_workspace_symbols` | Workspace symbol search |
| Introspection | `expand_cache_status` | Macro expansion cache status (Rust) |
| Natural Language | `sqry_ask` | NL → sqry command translation |

See [User Guide](USER_GUIDE.md#available-tools) for full parameters and examples.

Recent MCP behavior notes:
- `semantic_search` and related query tools accept normal sqry predicate
  strings; space-separated predicates are treated as implicit `AND`.
- `dependency_impact` and `is_node_in_cycle` accept file-path disambiguation
  for same-named symbols in different files.
- `find_duplicates` reports per-group truncation metadata when a duplicate
  group has more members than the display cap.
- `rebuild_index` can be routed through daemon-backed mode when `sqry-mcp` is
  started with `--daemon`.

### MCP Prompts (Claude Code)

When prompts are enabled, these appear as `/mcp__sqry__*` commands:

- `semantic_search`
- `find_callers`
- `find_callees`
- `trace_path`
- `explain_symbol`
- `code_impact`
- `ask`

### MCP Feature Flags

Disable specific tool groups via environment variables:

| Variable | Controls |
|----------|----------|
| `SQRY_MCP_ENABLE_GRAPH` | `trace_path`, `subgraph` |
| `SQRY_MCP_ENABLE_EXPORT` | `export_graph` |
| `SQRY_MCP_ENABLE_CROSS_LANGUAGE` | `cross_language_edges` |
| `SQRY_MCP_ENABLE_SEMANTIC_DIFF` | `semantic_diff` |
| `SQRY_MCP_ENABLE_DEPENDENCY_IMPACT` | `dependency_impact` |
| `SQRY_MCP_ENABLE_SQRY_ASK` | `sqry_ask` |

### rmcp-Based Rust Implementation

**Benefits**:
- ⚡ **Fast**: Type-safe, compiled performance
- 🔒 **Secure**: Path traversal protection, workspace isolation
- 📊 **Observable**: Tracing spans for all operations
- ✅ **Tested**: Comprehensive integration tests
- 🎛️ **Configurable**: Timeouts, limits, workspace root

---

## Configuration

### Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `SQRY_MCP_WORKSPACE_ROOT` | Workspace root directory | Current directory |
| `SQRY_MCP_TIMEOUT_MS` | Per-operation timeout | `60000` (60s) |
| `SQRY_MCP_INDEX_TIMEOUT_MS` | Index rebuild timeout | `600000` (10min) |
| `SQRY_MCP_MAX_OUTPUT_BYTES` | Output size limit | `50000` (50KB) |
| `SQRY_REDACTION_PRESET` | Response redaction level | `minimal` |
| `SQRY_INCLUDE_HIGH_COST` | Include high-cost plugins | `0` (disabled) |
| `SQRY_MCP_ENGINE_CACHE_CAPACITY` | Max cached workspace engines | `5` |
| `SQRY_MCP_DISCOVERY_CACHE_CAPACITY` | Max cached workspace paths | `100` |
| `SQRY_MCP_TRACE_CACHE_SIZE` | Max cached trace path results | `256` |
| `SQRY_MCP_SUBGRAPH_CACHE_SIZE` | Max cached subgraph results | `128` |

> Phase 3C DB21: the duplicated `SQRY_MCP_TRACE_PATH_CACHE_CAPACITY` /
> `SQRY_MCP_SUBGRAPH_CACHE_CAPACITY` / `SQRY_MCP_QUERY_CACHE_TTL_SECS`
> environment variables were retired. Use `SQRY_MCP_TRACE_CACHE_SIZE` /
> `SQRY_MCP_SUBGRAPH_CACHE_SIZE` instead; the payload-cache TTL is now
> fixed at 300 seconds.

### Cache Configuration (Optional)

The MCP server caches workspace engines, discovery results, and query results to improve performance across multiple repositories. Cache capacities can be tuned for your workload:

**Multi-workspace developers** (working across 10+ repositories):
```json
{
  "mcpServers": {
    "sqry": {
      "command": "/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_ENGINE_CACHE_CAPACITY": "10",
        "SQRY_MCP_DISCOVERY_CACHE_CAPACITY": "200"
      }
    }
  }
}
```

**Memory-constrained systems** (reduce cache sizes):
```json
{
  "mcpServers": {
    "sqry": {
      "command": "/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_ENGINE_CACHE_CAPACITY": "2",
        "SQRY_MCP_TRACE_CACHE_SIZE": "64",
        "SQRY_MCP_SUBGRAPH_CACHE_SIZE": "32"
      }
    }
  }
}
```

**Memory usage estimate**: ~10-15MB per cached workspace engine. Default settings (5 workspaces) use approximately 50-85MB total.

### Configuration Examples

**Increase timeout for large codebases**:
```json
{
  "mcpServers": {
    "sqry": {
      "command": "/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/project",
        "SQRY_MCP_TIMEOUT_MS": "60000"
      }
    }
  }
}
```

---

## Technical Details

### Architecture

**Protocol**: JSON-RPC 2.0 over stdio
**Transport**: Standard input/output
**Framing**: Newline-delimited JSON
**Logging**: stderr only (stdout reserved for JSON-RPC responses)

### Protocol Flow

```
AI Assistant
    ↓ stdin (JSON-RPC request)
sqry-mcp (Rust)
    ↓ spawns
sqry CLI (--json output)
    ↑ captures output
sqry-mcp (Rust)
    ↑ stdout (JSON-RPC response)
AI Assistant
```

### Performance

| Operation | Typical Time | Notes |
|-----------|-------------|-------|
| Fuzzy search | ~20ms | With warm index |
| Query | ~15ms | Boolean logic |
| Index status | ~5ms | Metadata read |
| Index build | ~2s | 10,000 symbols |

### Compatibility

| AI Assistant | Version | Status |
|-------------|---------|--------|
| Codex CLI | Latest | ✅ Tested |
| Claude Desktop | Latest | ✅ Tested |
| Windsurf | Latest | ✅ Tested |
| Cursor | Latest | ✅ Supported |
| Gemini CLI | Latest | ✅ Supported |

### Testing

Run the comprehensive Rust test suite:

```bash
cargo test -p sqry-mcp
```

Coverage includes:
- JSON-RPC protocol (errors, ID propagation, notifications)
- Newline-delimited JSON transport
- Tool validation and execution
- Path traversal security

**Manual testing**:
```bash
# List available tools (no JSON-RPC handshake required)
cargo run -q -p sqry-mcp -- --list-tools
```

### Implementation Notes

**Single implementation**:
- **rmcp-based Rust server** (default): Fast, secure, and tested; no legacy JSON-RPC loop

---

## Support & Feedback

**Need Help?**
- 📖 [User Guide](USER_GUIDE.md) - Complete documentation
- 🔧 [Troubleshooting](TROUBLESHOOTING.md) - Common issues and solutions
- 💬 [GitHub Discussions](https://github.com/verivus-oss/sqry/discussions)

**Found a Bug?**
- 🐛 [Report an issue](https://github.com/verivus-oss/sqry/issues)

**Want to Contribute?**
- See [Development](#development) section below
- Check out integration guides in this folder:
  - [Codex Integration](CODEX_INTEGRATION.md)
  - [Gemini Integration](GEMINI_INTEGRATION.md)
  - [Claude Code Workflow](CLAUDE_CODE_INTEGRATION.md)

---

## Development

### Adding New Tools

1. Add tool definition to `src/tools.rs`
2. Implement tool handler in `src/handlers/`
3. Add CLI invocation logic
4. Add integration tests to `tests/`
5. Update documentation

### Debugging

Enable debug logging:

```bash
RUST_LOG=debug cargo run -p sqry-mcp
```

Logs are written to stderr and visible in your AI assistant's logs.

---

## References

- **MCP Specification**: https://modelcontextprotocol.io/
- **sqry Documentation**: [docs/](../docs/)
- **Integration Guides**:
  - [Codex Integration](CODEX_INTEGRATION.md)
  - [Gemini Integration](GEMINI_INTEGRATION.md)
  - [Claude Code Workflow](CLAUDE_CODE_INTEGRATION.md)
  - [User Guide Setup Sections](USER_GUIDE.md#getting-started)

---

## License

MIT - See root LICENSE file

---

**Last Updated**: 2026-05-04
**Version**: 12.1.6
**Tested With**: sqry v4.8.2, Claude Desktop, Windsurf, Claude Code, Codex, Gemini CLI
