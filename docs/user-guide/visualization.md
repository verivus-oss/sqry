# Visualization Guide (Unified Graph)

Use `sqry visualize` to turn relation queries into diagrams. The command emits
Mermaid, Graphviz DOT, or D2 syntax for offline rendering.
It reads relation data from the unified graph snapshot. If the snapshot is
missing, `sqry visualize` auto-builds from source; running `sqry index` first is
recommended for large repos. If the graph is empty, the command returns an
actionable error; if no relations are found, it warns and still renders the root
context.

## Quick Examples

```bash
# Mermaid diagram of callers
sqry visualize "callers:main" --format mermaid --path .

# Graphviz DOT diagram of imports (save to file)
sqry visualize "imports:std" --format graphviz --output-file deps.dot --path .

# D2 diagram of callees
sqry visualize "callees:process" --format d2 --output-file graph.d2 --path .
```

## Relation Query Input

The `query` argument accepts relation queries such as:

- `callers:<symbol>`
- `callees:<symbol>`
- `imports:<substring>`
- `exports:<symbol>`

Imports use substring matching (for example, `imports:std` matches any import
containing `std`).
When multiple files import the same module, `imports:` aggregates all importers
in the diagram.
Exports resolve exported symbol names (functions/classes/modules), not a
separate export node.

When aliasing or special export forms are present, diagrams annotate edges with
labels such as `as <alias>`, `default`, `reexport`, or `namespace`.

Examples:

```bash
sqry visualize "callers:AuthService.login" --path .
sqry visualize "imports:std" --format d2 --path .
```

## Output Formats

- `--format mermaid` (default)
- `--format graphviz`
- `--format d2`

The output is plain text in the selected diagram syntax. You can render it
locally using Graphviz (`dot`), D2, or Mermaid tooling.

## Layout and Limits

- `--direction top-down | bottom-up | left-right | right-left`
- `--depth <n>` to control traversal depth (default: 3)
- `--max-nodes <n>` to cap diagram size (default: 100)

Example:

```bash
sqry visualize "callers:main" --depth 5 --max-nodes 200 --direction left-right --path .
```

## Graph Command Output

`sqry graph` accepts `--format` values such as `dot`, `mermaid`, and `d2`, but
those formats currently fall back to text output (warnings are emitted for some
operations). `sqry graph` is backed by the unified graph snapshot; use
`sqry visualize` when you need diagram output, and `sqry graph` for analytical
results.
