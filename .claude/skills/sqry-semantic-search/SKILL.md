---
name: sqry-semantic-search
version: 4.8.16
description: |
  AST-based semantic code search - understands code structure (functions, classes, types) and relationships (calls, imports, inheritance). Unlike embedding-based search that treats code as text, sqry parses code like a compiler to find symbols by what they ARE and DO.
---

# sqry Semantic Code Search Skill

Use this skill when users ask to:
- Find functions, classes, methods, or variables by name or kind
- Search for code with specific visibility (public/private)
- Find symbols in specific files or directories
- Trace call relationships (callers, callees, call paths)
- Analyze code dependencies and impact of changes
- Search for code by semantic properties rather than text patterns

## What Makes sqry Different

**sqry uses "semantic" in the compiler sense, not the NLP sense.**

| Aspect | Embedding-based Search | sqry |
|--------|----------------------|------|
| Meaning of "semantic" | Text meaning similarity | Code structure & relationships |
| Technology | ML embeddings + vector DB | AST parsing + graph analysis |
| Understands code structure | No - treats code as text | Yes - knows functions vs classes |
| Knows call relationships | No | Yes - callers, callees, imports |
| Query language | Natural language only | Structured predicates + NL |
| Requires ML models | Yes | No |

### The Key Insight

Other "semantic search" tools find code that *reads similarly* - searching "authenticate" finds "login", "verify", "auth_token" because they're semantically similar as English words.

sqry finds code by *what it is and does*:
- `kind:function name:*auth*` - Find functions with "auth" in the name
- `callers:authenticate` - Find everything that calls `authenticate()`
- `kind:class impl:Serialize` - Find classes implementing the Serialize trait
- `returns:Result` - Find functions returning Result types

**We parse code like a compiler does. We don't embed code like a document.**

## MCP Tools Reference

Claude uses MCP tools (not CLI commands) for code search. The sqry MCP server provides these tools:

### Core Search Tools

| Tool | Purpose | Required Params |
|------|---------|-----------------|
| `mcp__sqry__semantic_search` | Find symbols by query | `query` |
| `mcp__sqry__hierarchical_search` | RAG-optimized grouped results | `query` |
| `mcp__sqry__explain_code` | Get symbol details + relations | `file_path`, `symbol_name` |
| `mcp__sqry__search_similar` | Find similar symbols | `reference.file_path`, `reference.symbol_name` |

### Relation Tools

| Tool | Purpose | Required Params |
|------|---------|-----------------|
| `mcp__sqry__relation_query` | Query callers/callees/imports/exports | `symbol`, `relation_type` |
| `mcp__sqry__trace_path` | Find call paths between symbols | `from_symbol`, `to_symbol` |
| `mcp__sqry__dependency_impact` | Analyze change impact | `symbol` |

### Graph Tools

| Tool | Purpose | Required Params |
|------|---------|-----------------|
| `mcp__sqry__subgraph` | Extract context around symbols | `symbols` (array) |
| `mcp__sqry__export_graph` | Export graph in dot/d2/mermaid | `file_path` or `symbol_name` |
| `mcp__sqry__cross_language_edges` | Find cross-language calls | (none required) |
| `mcp__sqry__show_dependencies` | Show dependency tree | `file_path` or `symbol_name` |

### Analysis Tools

| Tool | Purpose | Required Params |
|------|---------|-----------------|
| `mcp__sqry__semantic_diff` | Compare code versions | `base.ref`, `target.ref` |
| `mcp__sqry__find_duplicates` | Find duplicate code patterns | (none required) |
| `mcp__sqry__find_cycles` | Find circular dependencies | (none required) |
| `mcp__sqry__find_unused` | Find unused/dead code | (none required) |
| `mcp__sqry__get_index_status` | Check index health | (none required) |
| `mcp__sqry__sqry_ask` | Natural language to sqry | `query` |

## Query Syntax

Queries use `field:value` predicates. Multiple predicates are AND-combined.

### Core Fields (22 total)

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Symbol name (exact or pattern) |
| `kind` | enum | function, method, class, struct, trait, enum, interface, module, variable, constant, type, namespace, property, parameter, import |
| `path` | path | File path (glob pattern). Alias: `file` |
| `lang` | string | Programming language. Alias: `language` |
| `parent` | string | Parent symbol name |
| `scope` | enum | file, module, class, function, block |
| `scope.type` | enum | Container type (module, function, class, method, etc.) |
| `scope.name` | string | Container name |
| `scope.parent` | string | Parent scope name |
| `scope.ancestor` | string | Any ancestor scope name |
| `text` | string | Full-text search in symbol body (NOT indexed - slow) |
| `repo` | string | Repository filter (for workspaces) |

### Relation Fields

| Field | Type | Description |
|-------|------|-------------|
| `callers` | string | Symbols that call X |
| `callees` | string | Symbols called by X |
| `imports` | string | Files that import X |
| `exports` | string | Files that export X |
| `returns` | string | Functions returning type X |
| `impl` | string | Types implementing trait X |
| `references` | string | Cross-file references to X |

### Static Analysis Fields

| Field | Type | Description |
|-------|------|-------------|
| `duplicates` | enum | body, function, signature, struct |
| `unused` | enum | public, private, function, struct, all |
| `circular` | enum | calls, imports, all |

## Common Query Patterns

### Finding Symbols

```
# By name
name:login
name:*Handler    # ends with Handler
name:get*        # starts with get

# By kind
kind:function
kind:class
kind:method

# Combine predicates (AND)
kind:function name:process
kind:class path:src/models
lang:rust kind:struct
```

### Relation Queries

```
# Who calls this function?
callers:authenticate

# What does this function call?
callees:processData

# Imports/exports
imports:database
exports:UserService

# Trait implementations
impl:Debug
impl:Serialize
```

### Scope Filtering

```
# Methods inside a specific class
kind:method scope.name:UserService

# Functions in a module
kind:function scope.type:module

# Nested in any ancestor
kind:method scope.ancestor:Api
```

## MCP Tool Examples

### semantic_search

```json
{
  "query": "kind:function name:*auth*",
  "filters": {
    "language": ["rust", "typescript"],
    "visibility": "public"
  },
  "max_results": 50
}
```

### relation_query

```json
{
  "symbol": "authenticate",
  "relation_type": "callers",
  "max_depth": 2
}
```

Relation types: `callers`, `callees`, `imports`, `exports`, `returns`

### trace_path

```json
{
  "from_symbol": "main",
  "to_symbol": "database_connect",
  "max_hops": 5,
  "cross_language": true
}
```

### hierarchical_search

Returns results grouped by file -> container -> symbol. Best for RAG:

```json
{
  "query": "kind:function",
  "path": "src/api",
  "max_files": 10,
  "auto_merge": true
}
```

### subgraph

Extract context around symbols for understanding:

```json
{
  "symbols": ["UserService", "authenticate"],
  "max_depth": 2,
  "include_callers": true,
  "include_callees": true
}
```

### dependency_impact

Before changing a symbol, check what would break:

```json
{
  "symbol": "shared_utility",
  "max_depth": 3,
  "include_indirect": true
}
```

### semantic_diff

Compare code between git refs:

```json
{
  "base": { "ref": "main" },
  "target": { "ref": "feature-branch" },
  "filters": {
    "change_types": ["added", "modified"],
    "symbol_kinds": ["function", "class"]
  }
}
```

### sqry_ask

Translate natural language to sqry:

```json
{
  "query": "find all public authentication functions"
}
```

### find_duplicates

Find duplicate code patterns (function bodies, signatures, or structs):

```json
{
  "duplicate_type": "body",
  "threshold": 80,
  "max_results": 50
}
```

Duplicate types: `body` (function bodies), `signature` (function signatures), `struct` (struct definitions)

### find_cycles

Find circular dependencies in the codebase:

```json
{
  "cycle_type": "calls",
  "min_depth": 2,
  "max_results": 50
}
```

Cycle types: `calls` (call cycles), `imports` (import cycles), `modules` (module cycles)

### find_unused

Find unused/dead code via reachability analysis:

```json
{
  "scope": "public",
  "language": ["rust"],
  "max_results": 100
}
```

Scopes: `public`, `private`, `function`, `struct`, `all`

## Supported Languages (35)

**Tier 1**: Rust, JavaScript, TypeScript, Python, Go, Java, C, C++, C#, PHP

**Tier 2**: Kotlin, Ruby, Swift, Scala, Lua, R, Dart, Elixir, Haskell, Perl, Zig, Groovy

**Domain-Specific**: SQL, Terraform, Puppet, Pulumi, Shell, HTML, CSS, Vue, Svelte

**Enterprise**: Salesforce Apex, SAP ABAP, ServiceNow (Xanadu), Oracle PL/SQL

## Filter Parameters

All search tools support these filters:

```json
{
  "filters": {
    "language": ["rust"],           // Limit to languages
    "visibility": "public",          // public or private
    "symbol_kind": ["function"],     // Filter by kinds
    "score_min": 0.7                 // Minimum relevance (0.0-1.0)
  }
}
```

## Pagination

Large result sets use cursor-based pagination:

```json
{
  "pagination": {
    "page_size": 50,
    "cursor": "<from previous response>"
  }
}
```

## Index Requirements

MCP tools require a pre-built index. If queries fail:

1. Check index status: `mcp__sqry__get_index_status`
2. Rebuild via CLI: `sqry index .` or `sqry index --force .`

## When NOT to Use sqry

Use other tools when:
- **Literal text search**: Use Grep tool for exact text patterns
- **File finding**: Use Glob tool for file names
- **Reading files**: Use Read tool for file contents
- **Code execution**: sqry only searches, doesn't run code

## Quick Tool Selection

**I know the symbol name and want to...**
- See its definition → `get_definition`
- See its signature/docs → `get_hover_info`
- See all references → `get_references`
- See who calls it → `direct_callers` (depth=1) or `relation_query` (multi-depth)
- See what it calls → `direct_callees` (depth=1) or `relation_query` (multi-depth)
- See call tree → `call_hierarchy` (set `max_depth` > 1 for deeper traversal)
- See its context + source → `explain_code`
- See what breaks if I change it → `dependency_impact`
- Check if it's in a cycle → `is_node_in_cycle`
- Find similar symbols → `search_similar`

**I want to search for symbols...**
- By name substring → `pattern_search`
- By name with ranking → `get_workspace_symbols`
- By kind/visibility/language → `semantic_search`
- With results grouped for RAG → `hierarchical_search`

**I want to analyze the codebase...**
- Find circular dependencies → `find_cycles`
- Find dead code → `find_unused`
- Find duplicate code → `find_duplicates`
- Compare git versions → `semantic_diff`
- Get overall stats → `get_graph_stats`
- Get health metrics → `get_insights`
- Get complexity scores → `complexity_metrics`

**I want to visualize/export...**
- Dependency tree → `show_dependencies`
- Subgraph around symbols → `subgraph`
- Graph as DOT/Mermaid/D2 → `export_graph`
- Call path between A and B → `trace_path`
- Cross-language edges → `cross_language_edges`

**I want to list everything...**
- All indexed files → `list_files`
- All indexed symbols → `list_symbols`
- All symbols in one file → `get_document_symbols`

## Recommended Workflow: Understand Before Changing

Follow **broad → narrow → impact** before modifying code.

### 1. Get the big picture
- `get_graph_stats` → node/edge counts, language breakdown, codebase size
- `get_insights` → health metrics (cycles, unused symbols, duplicates)
- `list_files` → indexed files, filtered by language

### 2. Find relevant symbols
- `semantic_search` → find by structure: `kind:function name:*auth*`
- `pattern_search` → substring match on names
- `get_workspace_symbols` → fuzzy name search with ranking
- `hierarchical_search` → results grouped by file/container (best for RAG context)

### 3. Understand a specific symbol
- `get_definition` → where is it defined?
- `get_hover_info` → signature, docs, type
- `explain_code` → full context + relations
- `get_document_symbols` → everything in the same file

### 4. Trace relationships before changing
- `direct_callers` → who calls this? (depth=1)
- `direct_callees` → what does it call? (depth=1)
- `relation_query` → callers/callees/imports/exports at multiple depths
- `dependency_impact` → what breaks if I change this?
- `show_dependencies` → dependency tree
- `subgraph` → extract context graph around symbols

### 5. Check quality concerns
- `find_cycles` → circular dependencies near the change
- `is_node_in_cycle` → is this specific symbol in a cycle?
- `find_unused` → dead code near the change area

### Example: changing function X

Steps use pseudo-call notation; pass named MCP JSON params in practice.

1. `get_definition(symbol: "X")` → find it
2. `explain_code(file_path: "...", symbol_name: "X")` → understand it
3. `dependency_impact(symbol: "X")` → what breaks if I change it
4. `direct_callers(symbol: "X")` → who depends on it
5. `get_document_symbols(file_path: "...")` → what else is in that file
6. Commit the change, then `semantic_diff(base: {ref: "main"}, target: {ref: "HEAD"})` to verify symbol-level deltas

## Additional Documentation

- [query-syntax.md](query-syntax.md) - Full predicate syntax reference
- [graph-queries.md](graph-queries.md) - Graph analysis details
- [examples.md](examples.md) - 50+ example queries
