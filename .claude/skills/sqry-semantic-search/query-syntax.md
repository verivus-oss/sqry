# sqry Query Syntax Reference

## Predicate Syntax

All predicates follow the pattern `field:value`. Multiple predicates are AND-combined.

## Operators

| Operator | Syntax | Example | Description |
|----------|--------|---------|-------------|
| Equal | `field:value` | `kind:function` | Exact match |
| Regex | `field~=pattern` | `name~=test.*` | Regular expression |

## Core Fields

### name - Symbol Name

```
# Exact match
name:login

# Glob patterns
name:*Handler        # ends with Handler
name:get*            # starts with get
name:*auth*          # contains auth

# Case sensitivity (default: case-sensitive)
name:Login           # matches "Login", not "login"
name:login           # matches "login", not "Login"

# Regex match
name~=^test_.*$
```

### kind - Symbol Type

```
# Single kind
kind:function
kind:method
kind:class
kind:struct
kind:trait
kind:enum
kind:interface
kind:module
kind:variable
kind:constant
kind:type
kind:namespace
kind:property
kind:parameter
kind:import
```

### path - File Path

```
# Directory filter
path:src/auth

# File filter
path:src/auth/login.rs

# Glob pattern
path:src/**/*.rs
path:*.ts

# Paths with spaces (use quotes)
path:"src/api services"

# Alias: file
file:src/api
```

### lang - Programming Language

```
# Filter by language
lang:rust
lang:typescript
lang:javascript
lang:python
lang:go
lang:java
lang:cpp
lang:csharp

# Alias: language
language:rust
```

### parent - Parent Symbol

```
# Methods in a specific class
parent:UserService

# Regex match
parent~=.*Controller$
```

### scope - Scope Type

```
# Basic scope types
scope:file
scope:module
scope:class
scope:function
scope:block
```

### scope.type - Container Type

```
# Specific container types
scope.type:function
scope.type:class
scope.type:method
scope.type:module
scope.type:struct
scope.type:trait
scope.type:impl
scope.type:interface
scope.type:enum
scope.type:namespace
scope.type:block
```

### scope.name - Container Name

```
# Inside specific container
scope.name:UserService

# Regex match
scope.name~=.*Service$
```

### scope.parent - Parent Scope Name

```
# Direct parent scope
scope.parent:ApiController
```

### scope.ancestor - Any Ancestor

```
# Any ancestor in scope chain
scope.ancestor:Database
```

### repo - Repository Filter (Workspaces)

```
# Glob pattern on repo name
repo:backend-*
repo:*-api
```

### text - Full-Text Search (NOT indexed)

```
# Search in symbol body (slow - NOT indexed)
text~=TODO.*fix
text~=deprecated
```

## Relation Fields

### callers - Who Calls X

```
# Direct callers
callers:authenticate

# With relation_query MCP tool, supports depth
```

### callees - What Does X Call

```
# What functions does X call
callees:processData
```

### imports - Who Imports X

```
# Files importing a module
imports:database
imports:lodash
```

### exports - Who Exports X

```
# Files exporting a symbol
exports:UserService
```

### returns - Functions Returning Type X

```
# Functions returning a type
returns:Promise
returns:Result

# Regex match
returns~=Vec<.*>
```

### impl - Trait Implementations

```
# Types implementing a trait
impl:Debug
impl:Serialize
impl:Clone
```

### references - Cross-File References

```
# Symbols with cross-file references
references:connect
```

## Static Analysis Fields

### duplicates - Find Duplicate Code

```
duplicates:body         # Duplicate function bodies
duplicates:function     # Duplicate functions
duplicates:signature    # Duplicate signatures
duplicates:struct       # Duplicate struct layouts
```

### unused - Find Unused Symbols

```
unused:public      # Unused public symbols
unused:private     # Unused private symbols
unused:function    # Unused functions
unused:struct      # Unused structs
unused:all         # All unused symbols
```

### circular - Find Circular Dependencies

```
circular:calls     # Mutual recursion
circular:imports   # Import cycles
circular:all       # All circular deps
```

## Combining Predicates

```
# All predicates are AND-combined
kind:function name:login
# Finds functions named "login"

kind:class path:src/models
# Finds classes in src/models

lang:rust kind:struct impl:Debug
# Finds Rust structs implementing Debug

kind:method scope.name:UserService
# Finds methods inside UserService
```

## Boolean Logic (CLI Query Command)

The CLI `sqry query` command supports boolean operators:

```bash
# AND (default)
sqry query "kind:function AND name:test"

# OR
sqry query "kind:class OR kind:struct"

# Combined
sqry query "lang:rust AND (visibility:public OR kind:trait)"
```

Note: MCP tools use filter parameters instead of boolean syntax.

## Quoting Rules

```
# Simple queries - quotes optional
name:login
kind:function

# Multiple predicates - quotes recommended
"name:login kind:function"

# Values with special characters - must quote
"path:src/api services"    # Space in path
"name:foo::bar"            # Colons in name
```

## MCP Tool Mapping

| Query Predicate | MCP Tool | Parameter |
|-----------------|----------|-----------|
| `callers:X` | `relation_query` | `symbol: "X", relation_type: "callers"` |
| `callees:X` | `relation_query` | `symbol: "X", relation_type: "callees"` |
| `imports:X` | `relation_query` | `symbol: "X", relation_type: "imports"` |
| `exports:X` | `relation_query` | `symbol: "X", relation_type: "exports"` |
| `returns:X` | `relation_query` | `symbol: "X", relation_type: "returns"` |

## Output Flags (CLI Only)

| Flag | Description | Example |
|------|-------------|---------|
| `--limit N` | Limit results | `--limit 10` |
| `--json` | JSON output | `--json` |
| `--csv` | CSV output | `--csv --headers` |
| `--preview N` | Show N context lines | `--preview 3` |
| `--no-color` | Disable colors | `--no-color` |
| `--sort FIELD` | Sort results | `--sort name` |

## Performance Notes

1. **Indexed fields** (fast): `kind`, `name`, `path`, `lang`, `callers`, `callees`, `imports`, `exports`, `returns`, `impl`, `references`

2. **Non-indexed fields** (slower): `text`, `scope`, `scope.*`, `repo`, `parent`, `duplicates`, `unused`, `circular`

3. **Combine indexed first**: Put indexed predicates before non-indexed ones for better performance:
   ```
   # Good: indexed first
   kind:function text~=TODO

   # Slower: non-indexed first
   text~=TODO kind:function
   ```
