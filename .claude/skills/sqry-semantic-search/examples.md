# sqry Query Examples

## Finding Functions

### By Name

```
name:login
name:authenticate
name:handle_request
```

### By Pattern

```
name:*Handler        # ends with Handler
name:get*            # starts with get
name:*auth*          # contains auth
name~=^test_.*       # regex: starts with test_
```

### With Filters

```
kind:function name:process
kind:function path:src/api
kind:function lang:rust
```

### MCP Tool

```json
{
  "query": "kind:function name:*auth*",
  "filters": { "visibility": "public" },
  "max_results": 50
}
```

## Finding Classes/Structs

### All Classes

```
kind:class
kind:struct
```

### By Name

```
name:User kind:class
name:*Service kind:class
name:*Repository kind:class
```

### In Specific Location

```
kind:class path:src/models
kind:struct path:src/types
```

### MCP Tool

```json
{
  "query": "kind:class",
  "path": "src/models",
  "filters": { "visibility": "public" }
}
```

## Finding Methods

### All Methods

```
kind:method
```

### By Name

```
name:process kind:method
name:validate* kind:method
```

### In Specific Class

```
kind:method scope.name:UserService
kind:method parent:ApiController
```

### MCP Tool

```json
{
  "query": "kind:method scope.name:UserService"
}
```

## Finding Variables/Constants

### All Constants

```
kind:constant
```

### By Name Pattern

```
name:*CONFIG* kind:constant
name:MAX_* kind:constant
```

### MCP Tool

```json
{
  "query": "kind:constant",
  "filters": { "visibility": "public" }
}
```

## Language-Specific Queries

### Rust

```
lang:rust kind:function
lang:rust kind:struct
lang:rust impl:Debug
lang:rust impl:Serialize
```

### TypeScript

```
lang:typescript kind:class
lang:typescript kind:interface
```

### Python

```
lang:python kind:function
lang:python kind:class
```

### MCP Tool

```json
{
  "query": "kind:function",
  "filters": { "language": ["rust", "go"] }
}
```

## Relation Queries

### Find Callers

```
callers:authenticate
callers:process_request
```

**MCP Tool:**
```json
{
  "symbol": "authenticate",
  "relation_type": "callers",
  "max_depth": 2
}
```

### Find Callees

```
callees:main
callees:initialize
```

**MCP Tool:**
```json
{
  "symbol": "main",
  "relation_type": "callees",
  "max_depth": 3
}
```

### Find Imports

```
imports:database
imports:lodash
imports:react
```

**MCP Tool:**
```json
{
  "symbol": "database",
  "relation_type": "imports"
}
```

### Find Exports

```
exports:UserService
exports:authenticate
```

**MCP Tool:**
```json
{
  "symbol": "UserService",
  "relation_type": "exports"
}
```

### Trace Paths

**MCP Tool:**
```json
{
  "from_symbol": "main",
  "to_symbol": "database_connect",
  "max_hops": 5,
  "cross_language": true
}
```

## Scope Filtering

### By Container Type

```
scope.type:class kind:method
scope.type:module kind:function
scope.type:impl kind:method
```

### By Container Name

```
scope.name:UserService
scope.name~=.*Controller$
```

### By Ancestor

```
scope.ancestor:Api
scope.ancestor:Database
```

### MCP Tool

```json
{
  "query": "kind:method scope.name:UserService"
}
```

## Static Analysis

### Find Duplicates

```
duplicates:body          # duplicate function bodies
duplicates:signature     # duplicate signatures
duplicates:struct        # duplicate struct layouts
```

### Find Unused Code

```
unused:public            # unused public symbols
unused:private           # unused private symbols
unused:function          # unused functions
unused:all               # all unused
```

### Find Circular Dependencies

```
circular:calls           # mutual recursion
circular:imports         # import cycles
circular:all             # all cycles
```

## Complex Queries (CLI)

### Boolean Logic

```bash
sqry query "kind:function AND name:test"
sqry query "kind:class OR kind:struct"
sqry query "lang:rust AND (kind:function OR kind:method)"
```

### Combined Filters

```bash
sqry query "kind:function path:src/api lang:typescript"
sqry query "impl:Debug kind:struct lang:rust"
```

## Common Workflows

### Understanding a New Codebase

```json
// Find entry points
{ "query": "name:main kind:function" }

// Find public APIs
{ "query": "kind:function", "filters": { "visibility": "public" } }

// List all classes
{ "query": "kind:class", "max_results": 50 }
```

### Investigating a Function

```json
// 1. Find the function
{ "query": "name:process_order kind:function" }

// 2. See what it calls
{ "symbol": "process_order", "relation_type": "callees" }

// 3. See who calls it
{ "symbol": "process_order", "relation_type": "callers" }

// 4. Get full context
{ "symbols": ["process_order"], "max_depth": 2 }
```

### Finding Related Code

```json
// All handlers
{ "query": "name:*Handler" }

// All services
{ "query": "name:*Service kind:class" }

// All tests
{ "query": "path:*test*" }
```

### Pre-Change Impact Analysis

```json
// What depends on this function?
{
  "symbol": "shared_utility",
  "max_depth": 3,
  "include_indirect": true
}
```

### Code Review Preparation

```json
// What changed?
{
  "base": { "ref": "main" },
  "target": { "ref": "HEAD" },
  "filters": { "change_types": ["added", "modified"] }
}
```

## Path and Repository Filters

### Directory Scope

```
path:src/auth
path:src/api/**
path:*.rs
```

### File-Specific

```
path:src/auth/login.rs
```

### Repository Filter (Workspaces)

```
repo:backend-*
repo:*-service
```

### MCP Tool with Path

```json
{
  "query": "kind:function",
  "path": "src/api"
}
```

## Output Formatting (CLI)

### JSON Output

```bash
sqry query "name:login" --json
sqry graph direct-callers "process" --json
```

### CSV Output

```bash
sqry query "kind:function" --csv --headers
sqry query "kind:class" --csv --columns name,file,line
```

### Limiting Results

```bash
sqry query "kind:function" --limit 10
sqry query "kind:class" --limit 5
```

### Preview Context

```bash
sqry query "name:main" --preview 3
sqry search "authenticate" --preview 5
```

## Hierarchical Search (RAG-Optimized)

```json
{
  "query": "kind:function name:*auth*",
  "max_files": 10,
  "max_containers_per_file": 5,
  "auto_merge": true,
  "include_file_context": true
}
```

## Similar Symbol Search

```json
{
  "reference": {
    "file_path": "src/auth/login.rs",
    "symbol_name": "authenticate"
  },
  "similarity_threshold": 0.7,
  "max_results": 20
}
```

## Natural Language (sqry_ask)

```json
{ "query": "find all public authentication functions" }
{ "query": "who calls the login method" }
{ "query": "show me all classes in the api folder" }
{ "query": "trace from main to database" }
```

## Error Troubleshooting

### Empty Results

1. Check if index exists: `mcp__sqry__get_index_status`
2. Try broader predicates: `kind:function` instead of specific name
3. Remove path filter to search everywhere
4. Verify language is indexed

### Index Not Found

```bash
# Build index
sqry index .

# Force rebuild
sqry index --force .
```

### Slow Queries

1. Use indexed fields first (kind, name, path, lang)
2. Avoid `text~=` unless necessary (not indexed)
3. Add path filters to narrow scope
4. Use `--limit` to cap results
