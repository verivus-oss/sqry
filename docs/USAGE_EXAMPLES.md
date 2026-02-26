# Usage Examples

Practical examples of sqry commands and queries organized by task. For query syntax details, see the [Query Syntax Reference](../README.md).

## Table of Contents

- [Getting Started](#getting-started)
- [Finding Symbols](#finding-symbols)
- [Navigating Relationships](#navigating-relationships)
- [Graph Analysis](#graph-analysis)
- [Output Formats](#output-formats)
- [Visualization](#visualization)
- [Cross-Language Analysis](#cross-language-analysis)
- [Real-World Workflows](#real-world-workflows)
- [CI/CD Integration](#cicd-integration)
- [MCP Tool Examples](#mcp-tool-examples)

---

## Getting Started

```bash
# Build the index (required before any query)
sqry index .

# Search for a symbol by name
sqry search "main" .

# Run a structured query
sqry query "kind:function AND name:test" .

# Get graph statistics
sqry graph stats
```

---

## Finding Symbols

### By Name

```bash
sqry query "name:login"              # segment match (matches "login", "MyClass.login")
sqry query "name~=Handler$"          # ends with Handler
sqry query "name~=^get"              # starts with get
sqry query "name~=auth"              # contains auth
sqry query "name~=^test_"            # starts with test_
```

### By Kind

```bash
sqry query "kind:function"
sqry query "kind:method"
sqry query "kind:class"
sqry query "kind:struct"
sqry query "kind:interface"
sqry query "kind:trait"
sqry query "kind:enum"
sqry query "kind:module"
sqry query "kind:constant"
```

### By Location

```bash
sqry query "kind:function AND path:src/api/**"
sqry query "kind:class AND path:src/models/**"
sqry query "kind:function AND path:*.rs"              # Rust files only
sqry query "kind:function AND path:src/auth/**/*.ts"  # TypeScript auth module
```

### By Language

```bash
sqry query "lang:rust AND kind:function"
sqry query "lang:typescript AND kind:class"
sqry query "lang:python AND kind:function"
sqry query "lang:go AND kind:struct"
```

### Combined Filters

```bash
sqry query "kind:function AND name:test"
sqry query "kind:class OR kind:struct"
sqry query "lang:rust AND (kind:function OR kind:method)"
sqry query "kind:function AND visibility:public AND name~=Handler$"
```

### By Scope

```bash
sqry query "scope.type:class AND kind:method"    # methods inside classes
sqry query "scope.name:UserService"              # symbols inside UserService
sqry query "scope.ancestor:Api"                  # nested under Api
sqry query "kind:method AND parent:ApiController" # methods of ApiController
```

### Fuzzy Search

```bash
sqry --fuzzy "patern" .      # finds "pattern", "Pattern", etc.
sqry --fuzzy "authn" .       # finds "authenticate", "auth_handler"
```

---

## Navigating Relationships

### Callers and Callees

```bash
# Who calls this function?
sqry query "callers:authenticate"

# What does this function call?
sqry query "callees:main"

# Combine with filters
sqry query "kind:function AND callers:process_data"
```

### Imports and Exports

```bash
sqry query "imports:database"
sqry query "imports:lodash"
sqry query "exports:UserService"
```

### Return Types

```bash
sqry query "returns:Result"
sqry query "returns:Promise"
```

### Type Implementations

```bash
sqry query "impl:Debug"
sqry query "impl:Serialize"
```

### Path Tracing

```bash
# Find the call path from main to a database function
sqry graph trace-path "main" "database_connect"

# JSON output for scripting
sqry graph --format json trace-path "main" "handle_error"

# Cross-language paths
sqry graph trace-path "api_handler" "db_query"
```

---

## Graph Analysis

### Statistics

```bash
sqry graph stats                          # overview
sqry graph call-chain-depth "main"        # max call depth
sqry graph dependency-tree "auth_module"  # transitive dependencies
```

### Cycle Detection

```bash
sqry cycles                               # all call cycles
sqry cycles --type imports                 # import cycles only
sqry cycles --type imports --min-depth 3   # only large import cycles
sqry graph is-in-cycle "build_graph"       # check a specific symbol
```

### Unused Code

```bash
sqry unused                               # all unused symbols
sqry unused --scope public                # unused public APIs
sqry unused --scope function --lang rust  # unused Rust functions
```

### Dependency Impact

```bash
sqry impact "authenticate" --depth 5 --show-files
```

### Similarity

```bash
sqry similar src/lib.rs process_data --threshold 0.8
sqry duplicates --type body --threshold 90
sqry duplicates --type signature
```

---

## Output Formats

```bash
# JSON
sqry query "kind:function" --json

# CSV with headers
sqry query "kind:class" --csv --headers

# TSV
sqry query "kind:function" --tsv

# Preview source context
sqry query "name:main" --preview 3

# Limit results
sqry query "kind:function" --limit 10

# No color (for piping)
sqry query "kind:function" --no-color
```

---

## Visualization

```bash
# Mermaid diagram
sqry visualize "callers:main" --format mermaid --path .

# Graphviz DOT
sqry visualize "imports:std" --format graphviz --output-file deps.dot --path .

# D2 diagram
sqry visualize "callees:process" --format d2 --output-file graph.d2 --path .

# Control layout
sqry visualize "callers:main" --depth 5 --max-nodes 200 --direction left-right --path .

# Export subgraph
sqry export --format mermaid --filter-lang rust,go --output graph.mmd
```

---

## Cross-Language Analysis

```bash
# List all cross-language relationships
sqry graph cross-language

# Filter by language and edge type (substring match on Debug names)
sqry graph cross-language --from-lang rust --edge-type ffi
sqry graph cross-language --from-lang sql --edge-type table
sqry graph cross-language --edge-type http

# Use --format on the parent graph command for JSON output
sqry graph --format json cross-language
```

---

## Real-World Workflows

### Understanding a New Codebase

```bash
# 1. Get the big picture
sqry graph stats
sqry query "kind:module" --limit 20

# 2. Find entry points
sqry query "name:main AND kind:function"

# 3. List public APIs
sqry query "kind:function AND visibility:public" --limit 20

# 4. Understand a module's structure
sqry query "kind:function AND path:src/auth/**"
```

### Investigating a Function

```bash
# 1. Find it
sqry query "name:process_order AND kind:function"

# 2. See what it calls
sqry query "callees:process_order"

# 3. See who calls it
sqry query "callers:process_order"

# 4. Get full context
sqry explain src/orders.rs process_order

# 5. Get surrounding symbols (positional args, --depth flag)
sqry subgraph process_order -d 2
```

### Before Making a Change

```bash
# 1. Check what depends on the symbol
sqry impact "shared_utility" --depth 3 --show-files

# 2. Check who calls it
sqry query "callers:legacy_function"

# 3. Trace paths from entry points
sqry graph trace-path "main" "legacy_function"

# 4. After the change, verify symbol-level deltas
sqry diff main HEAD
```

### Finding Related Code

```bash
sqry query "name~=Handler$"                     # all handlers
sqry query "name~=Service$ AND kind:class"      # all services
sqry query "name~=Repository$"                  # all repositories
sqry query "name~=test AND path:tests/**"        # test functions
```

---

## CI/CD Integration

### Enforce No New Cycles

```bash
CYCLES=$(sqry cycles --json | jq 'length')
if [ "$CYCLES" -gt 0 ]; then
  echo "Circular dependencies detected: $CYCLES cycles"
  exit 1
fi
```

### Enforce Architectural Boundaries

```bash
# UI code must never directly call database
# trace-path exits non-zero when no path exists, so capture the exit status
if TRACE_OUTPUT=$(sqry graph --format json trace-path render_ui db_execute 2>/dev/null); then
  if echo "$TRACE_OUTPUT" | jq -e '.path | length > 0' > /dev/null 2>&1; then
    echo "ERROR: UI code has direct path to database!"
    exit 1
  fi
fi
```

### Limit Unused Public Symbols

```bash
set -euo pipefail

UNUSED_COUNT=$(sqry unused --scope public --json | jq 'map(.count) | add // 0')
if [ "$UNUSED_COUNT" -gt 10 ]; then
  echo "Too many unused public symbols: $UNUSED_COUNT"
  exit 1
fi
```

### Watch for Changes

```bash
sqry watch --build            # watch with initial build
sqry watch --stats            # show update statistics
sqry watch --debounce 500     # custom debounce (ms)
```

---

## MCP Tool Examples

These examples show MCP tool calls as JSON payloads for AI assistant integrations.

### Search for Symbols

**Tool**: `semantic_search` — find public auth functions in Rust/TypeScript

```json
{
  "query": "kind:function AND name~=auth",
  "filters": {
    "language": ["rust", "typescript"],
    "visibility": "public"
  },
  "max_results": 50
}
```

### Query Relationships

**Tool**: `relation_query` — find all callers of authenticate (up to 2 levels deep)

```json
{
  "symbol": "authenticate",
  "relation_type": "callers",
  "max_depth": 2
}
```

### Trace Call Paths

**Tool**: `trace_path` — find how main reaches database_connect

```json
{
  "from_symbol": "main",
  "to_symbol": "database_connect",
  "max_hops": 5,
  "cross_language": true
}
```

### RAG-Optimized Search

**Tool**: `hierarchical_search` — search grouped by file for LLM consumption

```json
{
  "query": "kind:function",
  "path": "src/api",
  "max_files": 10,
  "auto_merge": true
}
```

### Impact Analysis

**Tool**: `dependency_impact` — what breaks if shared_utility changes

```json
{
  "symbol": "shared_utility",
  "max_depth": 3,
  "include_indirect": true
}
```

### Compare Git Versions

**Tool**: `semantic_diff` — symbol-level changes between branches

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

### Find Duplicates

**Tool**: `find_duplicates` — similar function bodies

```json
{
  "duplicate_type": "body",
  "threshold": 80,
  "max_results": 50
}
```

### Find Cycles

**Tool**: `find_cycles` — call cycles

```json
{
  "cycle_type": "calls",
  "min_depth": 2,
  "max_results": 50
}
```

### Find Unused Code

**Tool**: `find_unused` — unused public Rust symbols

```json
{
  "scope": "public",
  "language": ["rust"],
  "max_results": 100
}
```

### Extract Subgraph

**Tool**: `subgraph` — neighborhood around key symbols

```json
{
  "symbols": ["UserService", "authenticate"],
  "max_depth": 2,
  "include_callers": true,
  "include_callees": true
}
```
