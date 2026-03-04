# ServiceNow Xanadu Plugin (JavaScript)

ServiceNow-aware JavaScript plugin for sqry. Reuses the `tree-sitter-javascript` grammar and adds queries and post-processing tailored to ServiceNow constructs.

## Features

- Script Includes (Class.create pattern)
- GlideRecord constructor usage (captures table names)
- gs.* API calls (e.g., gs.info, gs.error)
- ES6 classes and methods
- Function declarations and var function expressions (dual-emit: variable + function)
- Metadata propagation: propagates GlideRecord table usage to enclosing functions/methods/classes

## Installation

Included in the sqry workspace. Build with:

```bash
cargo build -p sqry-lang-servicenow-xanadu
```

## Plugin-Specific Fields

- `has_gliderecord` (bool)
  - True if the symbol is a GlideRecord synthetic symbol, or has glide_table/uses_gliderecord metadata
- `glide_table` (string)
  - Table name on synthetic GlideRecord symbols (from constructor)
- `uses_gliderecord` (string)
  - Propagated table name used within the function/method/class body

## Example Queries

```bash
# Find functions using 'incident' table
sqry query "kind:function AND uses_gliderecord:incident"

# Find all GlideRecord usage
sqry query "has_gliderecord:true"

# Find classes working with 'task' table
sqry query "kind:class AND uses_gliderecord:task"
```

## Notes

- Selection strategy: The plugin currently advertises a `.snjs` extension to avoid conflicts with the generic JavaScript plugin. Future work may add project detection or config flags for `.js` takeover in ServiceNow projects.
- Propagation: Metadata is propagated via a post-pass that walks the AST from GlideRecord constructor sites to enclosing method/function/class nodes.
- Limitations: IIFE Business Rule patterns are planned (test present but ignored).

## Development

See the tests under `tests/servicenow_integration.rs` for examples and edge cases.
