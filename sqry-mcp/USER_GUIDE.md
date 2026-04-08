# sqry MCP Server - User Guide - by Verivus

**Version**: 7.2.0
**Last Updated**: 2026-04-08

Integrate sqry's semantic code search with AI assistants (Codex, Claude Desktop, Windsurf, Cursor, and others) via the Model Context Protocol (MCP).

---

## Response Redaction (Important)

If MCP responses are sent to hosted/external LLM providers, apply redaction
before transmission:

- Use [sqry-mcp-redaction](../sqry-mcp-redaction/README.md)
- Recommended preset: `standard`
- Higher protection: `strict` (hashes filenames, redacts code/docs)

This reduces leakage risk for absolute paths, workspace roots, and source
context.

---

## Table of Contents

1. [Overview](#overview)
2. [Installation](#installation)
3. [Getting Started](#getting-started)
4. [Available Tools](#available-tools)
5. [Configuration](#configuration)
6. [Common Workflows](#common-workflows)
7. [Query Syntax](#query-syntax)
8. [Tips & Best Practices](#tips--best-practices)
9. [Troubleshooting](#troubleshooting)

---

## Overview

### What is sqry MCP?

sqry MCP is a **Model Context Protocol server** that exposes sqry's semantic code search capabilities to AI coding assistants. It allows your AI assistant to:

- **Search** your codebase semantically (fuzzy, typo-tolerant)
- **Query** with structured filters (boolean logic, type filters)
- **Navigate** code relationships (callers, callees, imports, exports)
- **Analyze** dependencies and impact
- **Explain** code symbols with context
- **Compare** semantic changes between git commits

### Supported AI Assistants

| Assistant | Status | Notes |
|-----------|--------|-------|
| **Codex CLI** | ✅ Fully Supported | Auto-configured by `sqry mcp setup` |
| **Claude Code** | ✅ Fully Supported | Auto-configured by `sqry mcp setup` |
| **Gemini CLI** | ✅ Fully Supported | Auto-configured by `sqry mcp setup` |
| **Claude Desktop** | ✅ Fully Supported | Native integration |
| **Windsurf** | ✅ Fully Supported | Tested and verified |
| **Cursor** | ✅ Supported | MCP-compatible |
| **VS Code MCP Extension** | ✅ Supported | Via MCP protocol |

### Why Use sqry MCP?

**Better than grep/find**:
- Understands code structure (functions, classes, imports)
- Typo-tolerant fuzzy search
- Semantic relationships (who calls this function?)

**Better than text search**:
- Filters by visibility, async/sync, return types
- Cross-language understanding
- Structured queries with boolean logic

**Integrated with AI**:
- Natural language queries
- AI understands your codebase structure
- Faster context gathering for AI responses

---

## Installation

### Prerequisites

**Required**:
1. **sqry CLI** installed and in PATH
2. **AI Assistant** (Claude Code, Codex, Gemini CLI, Claude Desktop, Windsurf, or Cursor)
3. **Git** (for some features like semantic_diff)

### Step 1: Install sqry CLI and MCP Server

```bash
# Clone repository
git clone https://github.com/verivus-oss/sqry.git
cd sqry

# Install sqry CLI and MCP server
cargo install --path sqry-cli
cargo install --path sqry-mcp

# Verify installation
sqry --version
```

### Step 2: Index Your Project

```bash
cd /path/to/your/project
sqry index
```

### Step 3: Auto-Configure Your AI Tools

```bash
# Auto-configure all detected AI tools (Claude Code, Codex, Gemini CLI)
cd /path/to/your/project
sqry mcp setup

# Check what was configured
sqry mcp status
```

The `sqry mcp setup` command automatically detects installed tools and writes
the correct configuration:
- **Claude Code**: per-project entry with pinned workspace root
- **Codex/Gemini**: global entry using CWD-based workspace discovery

For more options: `sqry mcp setup --help`

### Manual Configuration

For AI tools not supported by `sqry mcp setup` (Claude Desktop, Windsurf, Cursor),
follow the manual setup guides below:
- [Claude Desktop](#claude-desktop-setup)
- [Windsurf](#windsurf-setup)
- [Cursor](#cursor-setup)

---

## Getting Started

### Codex CLI Setup

**Recommended** (auto-configure):

```bash
cd /path/to/your/project
sqry mcp setup --tool codex
sqry mcp status
```

`sqry mcp setup` writes a global entry in `~/.codex/config.toml`:

```toml
[mcp_servers.sqry]
command = "/absolute/path/to/sqry-mcp"
```

**Important**:
- Codex uses CWD-based workspace discovery by default.
- Start Codex from the project root you want sqry to analyze.
- `--workspace-root` is intentionally rejected for Codex to avoid pinning one repository globally.

### Gemini CLI Setup

**Recommended** (auto-configure):

```bash
cd /path/to/your/project
sqry mcp setup --tool gemini
sqry mcp status
```

`sqry mcp setup` writes a global entry in `~/.gemini/settings.json`:

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

**Important**:
- Gemini also supports project settings at `.gemini/settings.json`.
- sqry setup keeps Gemini on CWD-based workspace discovery by default.
- Start Gemini from the project root you want sqry to analyze.

### Claude Desktop Setup

**1. Locate Configuration File**:

- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Linux**: `~/.config/Claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

**2. Add sqry Server**:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry/target/release/sqry-mcp",
      "args": [],
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      }
    }
  }
}
```

**Important**:
- Use **absolute paths** (not `~` or relative paths)
- `SQRY_MCP_WORKSPACE_ROOT` is optional but recommended for security
- Replace `/path/to/your/project` with your actual project path

**3. Restart Claude Desktop**

Close and reopen Claude Desktop completely.

**4. Verify Connection**

In a new chat, type:
```
@sqry check if the server is running
```

You should see sqry tools available.

### Windsurf Setup

**1. Open Windsurf Settings**

- Click Settings (gear icon)
- Navigate to MCP Servers

**2. Add sqry Server**

```json
{
  "sqry": {
    "command": "/absolute/path/to/sqry/target/release/sqry-mcp",
    "env": {
      "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
    }
  }
}
```

**3. Reload Windsurf**

**4. Test**:
```
Use sqry to find all authentication functions
```

### Cursor Setup

**1. Enable MCP Support**

- Settings → Features → Enable MCP Protocol

**2. Add Server Configuration**

Create or edit `~/.cursor/mcp_settings.json`:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/absolute/path/to/sqry/target/release/sqry-mcp",
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/your/project"
      }
    }
  }
}
```

**3. Restart Cursor**

---

## Available Tools

sqry MCP provides **34 tools** for semantic code analysis. The authoritative schema for each tool is returned by `tools/list` or `sqry-mcp --list-tools`.

### Tool Catalog

| Category | Tool | Description |
|----------|------|-------------|
| Search | `semantic_search` | Advanced semantic code search with filters and pagination |
| Search | `hierarchical_search` | RAG-optimized hierarchical search (file → container → symbol) |
| Search | `pattern_search` | Substring match on symbol names |
| Relations | `relation_query` | Query callers, callees, imports, exports, returns |
| Relations | `call_hierarchy` | Call hierarchy tree for a symbol |
| Relations | `direct_callers` | Direct (single-hop) callers for a symbol |
| Relations | `direct_callees` | Direct (single-hop) callees for a symbol |
| Explain | `explain_code` | Explain a symbol with optional context and relations |
| Similarity | `search_similar` | Fuzzy match symbols similar to a reference |
| Dependencies | `show_dependencies` | Show dependency tree for a file or symbol |
| Graph | `export_graph` | Export graph as dot/d2/mermaid/json |
| Graph | `cross_language_edges` | Cross-language call edges |
| Graph | `trace_path` | Trace call paths between symbols |
| Graph | `subgraph` | Focused subgraph extraction around symbols |
| Analysis | `find_cycles` | Find cycles (calls/imports/modules) |
| Analysis | `is_node_in_cycle` | Check if a symbol participates in a cycle |
| Analysis | `find_duplicates` | Find duplicate code patterns |
| Analysis | `find_unused` | Find unused/dead code |
| Analysis | `dependency_impact` | Reverse dependency impact analysis |
| Analysis | `semantic_diff` | Compare semantic changes between git refs |
| Index | `get_index_status` | Index status and metadata |
| Index | `rebuild_index` | Rebuild unified graph index |
| Index | `list_files` | List indexed files (filter by language) |
| Index | `list_symbols` | List indexed symbols (filter by kind/language) |
| Index | `get_graph_stats` | Graph stats (nodes/edges/language distribution) |
| Index | `get_insights` | Codebase health indicators |
| Index | `complexity_metrics` | Code complexity metrics |
| Navigation | `get_definition` | Symbol definition location |
| Navigation | `get_references` | All references to a symbol |
| Navigation | `get_hover_info` | Hover info (signature/docs/type) |
| Navigation | `get_document_symbols` | Symbols in a specific file |
| Navigation | `get_workspace_symbols` | Workspace-wide symbol search |
| Introspection | `expand_cache_status` | Macro expansion cache status (Rust) |
| Natural Language | `sqry_ask` | Translate natural language into sqry commands |

## MCP Prompts (Claude Code)

When prompts are enabled, these appear as `/mcp__sqry__*` commands:

- `semantic_search`
- `find_callers`
- `find_callees`
- `trace_path`
- `explain_symbol`
- `code_impact`
- `ask`

### Tool Call Response Shape

`tools/call` returns a content-wrapped JSON payload. The `text` field contains the actual response object with `version`, `data`, and metadata fields.

```json
{
  "result": {
    "content": [
      {
        "type": "text",
        "text": "{\n  \"version\": \"2024-11-05\",\n  \"data\": { ... },\n  \"execution_ms\": 12\n}"
      }
    ],
    "isError": false
  }
}
```

Use `tools/list` to discover parameters and defaults for each tool.

Codex and Gemini usually invoke tools directly rather than slash prompt aliases.

## Configuration

### Environment Variables

| Variable | Purpose | Default | Example |
|----------|---------|---------|---------|
| `SQRY_MCP_WORKSPACE_ROOT` | Root directory for searches | Current directory | `/home/user/myproject` |
| `SQRY_MCP_MAX_OUTPUT_BYTES` | Max output size per response | 50000 | `100000` |
| `SQRY_MCP_TIMEOUT_MS` | Timeout per request | 60000 (60s) | `60000` |
| `SQRY_MCP_INDEX_TIMEOUT_MS` | Index rebuild timeout | 600000 (10min) | `600000` |
| `SQRY_REDACTION_PRESET` | Response redaction level | `minimal` | `none` |
| `SQRY_INCLUDE_HIGH_COST` | Include high-cost plugins | `0` | `1` |
| `SQRY_MCP_ENGINE_CACHE_CAPACITY` | Max cached workspace engines | 5 | `10` |
| `SQRY_MCP_DISCOVERY_CACHE_CAPACITY` | Max cached workspace paths | 100 | `200` |
| `SQRY_MCP_TRACE_PATH_CACHE_CAPACITY` | Max cached trace path results | 256 | `512` |
| `SQRY_MCP_SUBGRAPH_CACHE_CAPACITY` | Max cached subgraph results | 128 | `256` |
| `SQRY_MCP_QUERY_CACHE_TTL_SECS` | Query cache TTL in seconds | 300 | `600` |

### Cache Configuration

The MCP server caches workspace engines, discovery results, and query results to improve performance when working with multiple repositories. Caches use LRU (Least Recently Used) eviction and are automatically invalidated when indexes are rebuilt.

**Understanding cache capacities**:
- **Engine cache**: Each entry loads a complete code graph for a workspace (~10-15MB per workspace)
- **Discovery cache**: Maps file paths to workspace roots (minimal memory, ~100 bytes per entry)
- **Query caches**: Store results from trace_path and subgraph queries (size varies by query complexity)

**Default settings** work well for developers working on 1-5 repositories simultaneously. Adjust if you:
- Work across 10+ repositories regularly → increase engine cache capacity
- Have memory constraints → reduce all cache capacities
- Notice stale results → reduce query cache TTL

**Example configurations**:

```json
{
  "mcpServers": {
    "sqry": {
      "command": "/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/project",
        "SQRY_MCP_ENGINE_CACHE_CAPACITY": "10",
        "SQRY_MCP_DISCOVERY_CACHE_CAPACITY": "200",
        "SQRY_MCP_QUERY_CACHE_TTL_SECS": "600"
      }
    }
  }
}
```

**Memory usage estimate**: Approximately `engine_cache_capacity × 10-15MB` + `5-10MB` for query caches. Default settings (5 engines) use 50-85MB total.

**Cache isolation**: Caches are automatically isolated per workspace. Querying repository A will never return cached results from repository B, even for identically-named symbols.

### Feature Flags

Disable specific MCP tool groups via environment variables:

| Variable | Controls |
|----------|----------|
| `SQRY_MCP_ENABLE_GRAPH` | `trace_path`, `subgraph` |
| `SQRY_MCP_ENABLE_EXPORT` | `export_graph` |
| `SQRY_MCP_ENABLE_CROSS_LANGUAGE` | `cross_language_edges` |
| `SQRY_MCP_ENABLE_SEMANTIC_DIFF` | `semantic_diff` |
| `SQRY_MCP_ENABLE_DEPENDENCY_IMPACT` | `dependency_impact` |
| `SQRY_MCP_ENABLE_SQRY_ASK` | `sqry_ask` |

### Security: Workspace Root

**Recommended**: Always set `SQRY_MCP_WORKSPACE_ROOT` to prevent path traversal:

```json
{
  "env": {
    "SQRY_MCP_WORKSPACE_ROOT": "/absolute/path/to/project"
  }
}
```

**Without this**:
- AI can access any directory on your system
- Potential security risk

**With this**:
- All paths restricted to workspace root
- Path traversal attempts blocked

### Performance Tuning

**Large Codebases (10,000+ symbols)**:

```json
{
  "env": {
    "SQRY_MCP_TIMEOUT_MS": "60000",
    "SQRY_MCP_MAX_OUTPUT_BYTES": "100000"
  }
}
```

**Memory-Constrained Systems**:

```json
{
  "env": {
    "SQRY_MCP_MAX_OUTPUT_BYTES": "25000"
  }
}
```

### Multiple Projects

**Option 1: Multiple Server Instances**

```json
{
  "mcpServers": {
    "sqry-project-a": {
      "command": "/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/project-a"
      }
    },
    "sqry-project-b": {
      "command": "/path/to/sqry-mcp",
      "env": {
        "SQRY_MCP_WORKSPACE_ROOT": "/path/to/project-b"
      }
    }
  }
}
```

**Option 2: Dynamic Path in Prompts**

```
@sqry search for auth in /path/to/project-a
```

---

## Common Workflows

### Workflow 1: Understanding a New Codebase

**Goal**: Quickly understand how a feature works

**Steps**:

1. **Get overview**:
   ```
   @sqry show me all functions related to authentication
   ```

2. **Find entry points**:
   ```
   @sqry find functions that call authenticate
   ```

3. **Explore dependencies**:
   ```
   @sqry show dependencies for src/auth/service.ts
   ```

4. **Understand flow**:
   ```
   @sqry explain the authenticate function in src/auth/service.ts
   ```

### Workflow 2: Refactoring Safely

**Goal**: Change a function without breaking things

**Steps**:

1. **Find all callers**:
   ```
   @sqry find all callers of processUser
   ```

2. **Analyze impact**:
   ```
   @sqry analyze impact of changing processUser
   ```

3. **Check dependencies**:
   ```
   @sqry show dependencies for the module containing processUser
   ```

4. **Verify no surprises**:
   ```
   @sqry find functions similar to processUser
   ```

### Workflow 3: Code Review

**Goal**: Review changes in a pull request

**Steps**:

1. **See what changed**:
   ```
   @sqry compare main and feature-branch
   ```

2. **Check for added functions**:
   ```
   @sqry show functions added between main and feature-branch
   ```

3. **Verify dependencies**:
   ```
   @sqry show dependencies for new files
   ```

4. **Check impact**:
   ```
   @sqry analyze impact of changed functions
   ```

### Workflow 4: Debugging

**Goal**: Find where a bug might be

**Steps**:

1. **Find related code**:
   ```
   @sqry search for error handling in the payment module
   ```

2. **Trace execution**:
   ```
   @sqry find all callers of validatePayment
   ```

3. **Check similar bugs**:
   ```
   @sqry find functions similar to handlePaymentError
   ```

4. **Review dependencies**:
   ```
   @sqry show dependencies for src/payment/service.ts
   ```

### Workflow 5: Finding Examples

**Goal**: Learn how to use a pattern

**Steps**:

1. **Find examples**:
   ```
   @sqry find all async error handlers
   ```

2. **Get details**:
   ```
   @sqry explain handleAsyncError in src/utils/errors.ts
   ```

3. **Find similar patterns**:
   ```
   @sqry find functions similar to handleAsyncError
   ```

4. **Check usage**:
   ```
   @sqry find all callers of handleAsyncError
   ```

---

## Query Syntax

### Structured Query Tool

When using `relation_query` or other query tools, you can use structured query syntax:

### Basic Queries

**By Kind**:
```
kind:function         # All functions
kind:class           # All classes
kind:method          # All methods
kind:interface       # All interfaces
```

**By Name**:
```
name:authenticate    # Exact match
name~=/auth/        # Regex: contains "auth"
name~=/^test/       # Regex: starts with "test"
name~=/Handler$/    # Regex: ends with "Handler"
```

**By Attribute**:
```
async:true          # Async functions
visibility:public   # Public symbols
static:true         # Static methods
abstract:true       # Abstract classes/methods
```

### Boolean Logic

**AND**:
```
kind:function AND async:true
kind:method AND visibility:private
name~=/test/ AND kind:function
```

**OR**:
```
kind:function OR kind:method
name~=/error/ OR name~=/handle/
visibility:public OR visibility:protected
```

**NOT**:
```
kind:function NOT name~=/private/
async:true NOT visibility:private
kind:class NOT abstract:true
```

**Grouping**:
```
(kind:function OR kind:method) AND async:true
kind:class AND (visibility:public OR visibility:protected)
```

### Query Predicates

These string predicates go in the `query` parameter:

**File-based**:
```
file:./src/auth.ts
file~=/src\/.*\.ts$/
file~=/test|spec/
```

**Return types**:
```
returns:Promise
returns~=/Result<.*>/
returns:void
```

**Language**:
```
lang:typescript
lang:python
lang:rust
```

### Structured Filters Parameter

For simple constraints, you can also use the `filters` parameter with a JSON object instead of (or in addition to) query predicates:

```json
{
  "language": ["rust", "typescript"],
  "symbol_kind": ["function", "method"],
  "visibility": "public",
  "score_min": 0.5
}
```

All fields are optional. Filters are applied server-side before query evaluation.

**Comparison: Query Predicates vs Filters Parameter**

| Feature | Query Predicates (`query`) | Structured Filters (`filters`) |
|---------|---------------------------|-------------------------------|
| Syntax | String: `lang:rust kind:function` | JSON object: `{"language":["rust"]}` |
| Boolean logic | AND, OR, NOT, grouping | Implicit AND only |
| Regex | `name~=/pattern/` | Not supported |
| Globs | `name:parse*`, `file:src/**` | Not supported |
| Numeric | `lines>100` | `score_min` only |
| Best for | Complex, expressive queries | Simple pre-filtering |

You can combine both: use `query` for complex expressions and `filters` for structured constraints.

---

## Tips & Best Practices

### Indexing Tips

**1. Build index before using MCP**:
```bash
cd /your/project
sqry index .
```

**2. Keep index fresh**:
- Rebuild after major changes
- Index rebuilds are fast (2-5 seconds for 10K symbols)

**3. Verify index**:
```
@sqry check index status
```

### Search Tips

**1. Start with fuzzy search**:
```
# Good for exploration
@sqry search for authentication
```

**2. Use structured queries for precision**:
```
# Good for specific needs
@sqry find all public async functions
```

**3. Combine with AI understanding**:
```
# Let AI interpret and search
@sqry find all error handlers that might need updating
```

### Performance Tips

**1. Limit results for faster responses**:
```
@sqry search for functions (limit to 20 results)
```

**2. Use specific queries**:
```
# Slow: search for all functions
# Fast: search for auth functions in src/auth/
```

**3. Increase timeout for complex queries**:
```json
{
  "env": {
    "SQRY_MCP_TIMEOUT_MS": "60000"
  }
}
```

### AI Prompt Tips

**1. Be specific about what you want**:
```
# Vague: find auth stuff
# Better: find all authentication functions in the API layer
```

**2. Use tool names when needed**:
```
# Explicit: use sqry to find callers of authenticate
# Implicit: who calls authenticate?  (AI figures it out)
```

**3. Ask for explanations**:
```
@sqry explain how authentication works
# AI will use multiple tools to build comprehensive answer
```

**4. Combine with git context**:
```
@sqry what authentication changes were made in the last commit?
# Uses semantic_diff tool
```

### Security Tips

**1. Always set workspace root**:
```json
{
  "env": {
    "SQRY_MCP_WORKSPACE_ROOT": "/absolute/path/to/project"
  }
}
```

**2. Don't expose sensitive codebases**:
- Only configure for projects you're working on
- Remove MCP config when done with a project

**3. Review AI queries**:
- Check what the AI is searching for
- Verify paths make sense

---

## Troubleshooting

### Quick Diagnostics

**1. Check server is running**:
```
@sqry check if the server is running
```

**2. Check index exists**:
```bash
cd /your/project
ls -la .sqry-index
```

**3. Test binary**:
```bash
sqry --version
# Should output: sqry 7.2.0+
```

**4. Check logs** (AI assistant specific):
- **Claude Desktop**: Check logs in `~/Library/Logs/Claude/`
- **Windsurf**: Check developer console
- **Cursor**: Check Output panel

### Common Issues

**Issue**: "sqry not found"

**Solution**: Install sqry CLI and ensure it's in PATH:
```bash
cargo install --path sqry-cli
which sqry  # Verify location
```

**Issue**: "No index found"

**Solution**: Build index first:
```bash
cd /your/project
sqry index .
```

**Issue**: MCP server not connecting

**Solutions**:
1. Check JSON syntax in config
2. Use absolute paths (not `~`)
3. Restart AI assistant completely
4. Check server binary exists: `ls -l /path/to/sqry-mcp`

**Issue**: Timeout errors

**Solution**: Increase timeout:
```json
{
  "env": {
    "SQRY_MCP_TIMEOUT_MS": "60000"
  }
}
```

**Issue**: Empty results

**Solutions**:
1. Rebuild index: `sqry index --force .`
2. Check index status: `sqry index --status .`
3. Verify file is in supported language
4. Try broader search

For more detailed troubleshooting, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md).

---

## More Help

### Documentation

- **[README](README.md)** - Technical overview
- **[Codex Integration](CODEX_INTEGRATION.md)** - Codex-specific setup and workflow guidance
- **[Gemini Integration](GEMINI_INTEGRATION.md)** - Gemini-specific setup and workflow guidance
- **[Troubleshooting](TROUBLESHOOTING.md)** - Detailed issue resolution
- **Integration Guides**:
  - Claude Code Workflow: [CLAUDE_CODE_INTEGRATION.md](CLAUDE_CODE_INTEGRATION.md)
  - Claude Desktop Setup: [Getting Started](#claude-desktop-setup)
  - Windsurf Setup: [Getting Started](#windsurf-setup)
  - Cursor Setup: [Getting Started](#cursor-setup)

### Support

- **GitHub Issues**: https://github.com/verivus-oss/sqry/issues
- **Discussions**: Ask questions and share workflows

### Feedback

sqry MCP is production-ready but evolving. Your feedback helps!

- Report bugs via GitHub Issues
- Suggest features via GitHub Discussions
- Share your workflows and use cases

---

**Last Updated**: 2026-04-08
**MCP Server Version**: 7.2.0
**Protocol**: MCP 2024-11-05 (JSON-RPC 2.0)
**sqry CLI Required**: 7.2.0+
