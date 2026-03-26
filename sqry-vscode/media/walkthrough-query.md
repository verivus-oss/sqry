# Run Structured Queries

sqry's query language lets you filter symbols by kind, language, visibility, and relationships.
It is far more precise than plain name search.

## How to Query

- **Keyboard shortcut**: `Ctrl+Alt+Q` (Mac: `Cmd+Alt+Q`)
- **Command Palette**: `Ctrl+Shift+P` → `Sqry: Query`

## Query Syntax

Combine filters with `AND`, `OR`, and `NOT`. Filters are `key:value` pairs.

## Example Queries

**Find all parse functions:**
```
kind:function AND name:parse
```

**Who calls handleRequest?**
```
callers:handleRequest
```

**Functions that return Result:**
```
returns:Result
```

**Rust structs and classes:**
```
kind:class AND lang:rust
```

**Public functions in a specific file:**
```
kind:function AND visibility:public AND file:src/api
```

**Interfaces that extend another interface:**
```
kind:interface AND inherits:Serializable
```

## Supported Filter Keys

| Key | Description |
|-----|-------------|
| `kind` | Symbol kind: `function`, `class`, `method`, `struct`, `trait`, `interface`, `constant`, `type` |
| `name` | Symbol name (substring match) |
| `lang` | Language: `rust`, `python`, `typescript`, `go`, `java`, ... |
| `visibility` | `public`, `private`, `protected` |
| `file` | File path (substring match) |
| `callers` | Symbols that call the given name |
| `callees` | Symbols called by the given name |
| `returns` | Return type (substring match) |
| `inherits` | Parent class or interface |

Results appear in the Sqry sidebar panel. Click any result to navigate to its definition.
