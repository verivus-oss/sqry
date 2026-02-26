# sqry Graph Queries Reference

Graph queries analyze call relationships and dependencies between symbols. They leverage sqry's unified graph architecture.

## MCP Graph Tools

### relation_query - Find Related Symbols

Query callers, callees, imports, exports, or return types.

```json
{
  "symbol": "authenticate",
  "relation_type": "callers",
  "max_depth": 2,
  "max_results": 100
}
```

**Parameters:**
- `symbol` (required): Symbol name or qualified identifier
- `relation_type` (required): `callers`, `callees`, `imports`, `exports`, `returns`
- `max_depth`: 1-5, default 1
- `max_results`: 1-5000, default 200
- `path`: Workspace-relative directory, default "."

**Relation Types:**
| Type | Description |
|------|-------------|
| `callers` | Symbols that call X |
| `callees` | Symbols called by X |
| `imports` | Files/modules that import X |
| `exports` | Files/modules that export X |
| `returns` | Functions returning type X |

### trace_path - Find Call Paths

Find shortest or top-K paths between two symbols.

```json
{
  "from_symbol": "main",
  "to_symbol": "database_connect",
  "max_hops": 5,
  "max_paths": 5,
  "cross_language": true,
  "min_confidence": 0.5
}
```

**Parameters:**
- `from_symbol` (required): Starting symbol
- `to_symbol` (required): Target symbol
- `max_hops`: 1-10, default 5
- `max_paths`: 1-20, default 5 (top-K ranked)
- `cross_language`: Allow JS->Python->C++ paths, default true
- `min_confidence`: 0.0-1.0, default 0.5

### subgraph - Extract Context

Extract a focused subgraph around seed symbols.

```json
{
  "symbols": ["UserService", "authenticate"],
  "max_depth": 2,
  "max_nodes": 50,
  "include_callers": true,
  "include_callees": true,
  "include_imports": false,
  "cross_language": true
}
```

**Parameters:**
- `symbols` (required): Array of seed symbol names
- `max_depth`: 1-5, default 2
- `max_nodes`: 1-500, default 50
- `include_callers`: default true
- `include_callees`: default true
- `include_imports`: default false
- `cross_language`: default true

### export_graph - Generate Diagrams

Export dependency graphs in various formats.

```json
{
  "file_path": "src/auth/login.rs",
  "format": "mermaid",
  "max_depth": 2,
  "include": ["calls", "imports"],
  "verbose": true
}
```

**Parameters:**
- `file_path` or `symbol_name` (one required)
- `format`: `json`, `dot`, `d2`, `mermaid`
- `max_depth`: 1-5, default 2
- `include`: Array of `calls`, `imports`, `exports`, `returns`
- `verbose`: Include extra details
- `languages`: Filter by languages

**Output Formats:**
| Format | Use Case |
|--------|----------|
| `json` | Programmatic processing |
| `dot` | Graphviz visualization |
| `d2` | D2 diagrams |
| `mermaid` | Markdown-compatible diagrams |

### cross_language_edges - Find Polyglot Calls

List calls that cross language boundaries.

```json
{
  "from_lang": "TypeScript",
  "to_lang": "Python",
  "max_results": 100
}
```

**Parameters:**
- `from_lang`: Optional caller language filter
- `to_lang`: Optional callee language filter
- `max_results`: 1-5000, default 500

### show_dependencies - Dependency Tree

Show transitive dependencies for a file or symbol.

```json
{
  "file_path": "src/main.rs",
  "symbol_name": "main",
  "max_depth": 3
}
```

**Parameters:**
- `file_path` or `symbol_name` (at least one required)
- `max_depth`: 1-5, default 2
- `max_results`: 1-5000, default 500

### dependency_impact - Change Analysis

Analyze what would break if a symbol is changed.

```json
{
  "symbol": "shared_utility",
  "max_depth": 3,
  "include_files": true,
  "include_indirect": true
}
```

**Parameters:**
- `symbol` (required): Symbol to analyze
- `max_depth`: 1-10, default 3
- `include_files`: Include affected file paths, default true
- `include_indirect`: Include transitive deps, default true

### semantic_diff - Version Comparison

Compare code between git refs.

```json
{
  "base": { "ref": "main" },
  "target": { "ref": "feature-branch" },
  "filters": {
    "change_types": ["added", "modified", "removed"],
    "symbol_kinds": ["function", "class"]
  },
  "include_unchanged": false
}
```

**Parameters:**
- `base.ref` (required): Git ref for base version
- `target.ref` (required): Git ref for target version
- `base.file_path`: Optional file filter
- `target.file_path`: Optional file filter
- `include_unchanged`: default false
- `filters.change_types`: `added`, `removed`, `modified`, `renamed`, `signature_changed`
- `filters.symbol_kinds`: Filter by symbol kinds

## CLI Graph Commands

The CLI provides equivalent graph functionality:

```bash
# Trace path between symbols
sqry graph trace-path main database_connect

# Show call depth
sqry graph call-chain-depth process_request

# Show dependency tree
sqry graph dependency-tree auth_module

# List graph nodes
sqry graph nodes --format json

# List graph edges
sqry graph edges --kind calls

# Cross-language relationships
sqry graph cross-language

# Graph statistics
sqry graph stats

# Detect cycles
sqry graph cycles

# Code complexity
sqry graph complexity
```

**Common Flags:**
- `--format`: `text`, `json`, `dot`, `mermaid`, `d2`
- `--path`: Directory to search
- `--verbose`: Detailed output

## Common Use Cases

### Understanding a Function

```json
// 1. Who calls it?
{ "symbol": "process_order", "relation_type": "callers", "max_depth": 2 }

// 2. What does it call?
{ "symbol": "process_order", "relation_type": "callees" }

// 3. Get full context
{
  "symbols": ["process_order"],
  "include_callers": true,
  "include_callees": true,
  "max_depth": 2
}
```

### Impact Analysis

```json
// Before changing shared_utility
{
  "symbol": "shared_utility",
  "max_depth": 3,
  "include_indirect": true
}
```

### Tracing Data Flow

```json
// How does user input reach the database?
{
  "from_symbol": "handle_user_input",
  "to_symbol": "execute_query",
  "max_hops": 8
}
```

### Finding Entry Points

```json
// Symbols with no callers (potential entry points)
{ "symbol": "target_function", "relation_type": "callers" }
// Empty result = no callers = entry point
```

### Cross-Language Analysis

```json
// Find all TypeScript -> Python calls
{
  "from_lang": "TypeScript",
  "to_lang": "Python"
}
```

### Pre-Commit Review

```json
// What changed between branches?
{
  "base": { "ref": "main" },
  "target": { "ref": "HEAD" },
  "filters": {
    "change_types": ["added", "modified"],
    "symbol_kinds": ["function"]
  }
}
```

## Edge Types in Unified Graph

sqry's unified graph tracks 20+ edge types:

**Structural:**
- `Defines` - Symbol definition
- `Contains` - Parent-child containment

**References:**
- `Calls` - Function/method calls (with argument count, async flag)
- `References` - General references
- `Imports` - Import statements (with alias, wildcard flag)
- `Exports` - Export statements (with kind, alias)
- `TypeOf` - Type annotations

**OOP:**
- `Inherits` - Class inheritance
- `Implements` - Interface/trait implementation

**Cross-Language:**
- `FfiCall` - Foreign function interface
- `HttpRequest` - HTTP API calls
- `GrpcCall` - gRPC calls
- `WebAssemblyCall` - WASM calls
- `DbQuery` - Database queries

**Extended:**
- `MessageQueue` - Message queue interactions
- `WebSocket` - WebSocket communications
- `GraphQLOperation` - GraphQL operations
- `ProcessExec` - Process execution
- `FileIpc` - File-based IPC
- `ProtocolCall` - Protocol-based calls

## Limitations

1. **Dynamic dispatch**: Virtual/dynamic calls may not be fully resolved
2. **Indirect calls**: Function pointers/callbacks may not be traced
3. **External calls**: Calls to external libraries shown as leaf nodes
4. **Reflection**: Runtime-determined calls not captured
5. **Macros**: Some macro-generated code may have incomplete edges
