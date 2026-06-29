# Advanced Analysis

This guide covers graph predicates and analysis workflows for users who need more than symbol search.

## Edge-Backed Return Queries

`returns:<TypeName>` matches functions and methods with an outgoing `TypeOf{Return}` edge to a type node whose name equals `TypeName`.

```bash
sqry query "kind:function AND returns:Result"
sqry plan-query "kind:function returns:Promise"
```

The predicate is edge-backed, not signature-text-backed. It is exact and case-sensitive.

## Resolution-Aware Calls

`resolved_via:<kind>` filters call relationships by dispatch provenance where the graph has populated that metadata.

Accepted values:

- `direct`
- `type_match`
- `binding_plane`
- `virtual_dispatch`
- `interface_dispatch`
- `duck_typed`
- `structural`
- `promiscuous_elided`

Examples:

```bash
sqry plan-query "kind:function callers:my_read resolved_via:binding_plane"
sqry plan-query "kind:function callers:main resolved_via:direct"
```

Population varies by language and resolver. Treat missing provenance as absent metadata, not proof that a call cannot exist.

## Framework Filters

MCP tool schemas expose a typed `framework` parameter for framework-aware filtering surfaces. The text grammar form `framework:<id>` is not documented as generally available here; rely on MCP parameters or verify parser support in your installed version before using text grammar.

## Definition-Only Queries

Planner/query surfaces that document definition-only filtering accept `items:true` or `is_definition:true` as query predicates:

```bash
sqry query "kind:function items:true"
sqry plan-query "kind:function is_definition:true"
```

For MCP `list_symbols`, use the structured parameter instead of a query predicate:

```json
{
  "kind": "function",
  "language": "rust",
  "items_only": true
}
```

Snapshots before V16 do not carry trustworthy definition signal. When a pre-V16 graph is loaded, definition-only query predicates and `list_symbols.items_only` return a `reindexRequired` / reindex-required advisory instead of silently evaluating stale default-false marker data. Rebuild with `sqry index --force .`.

## Context Propagation

Go context propagation analysis detects call sites where `context.Context` is available but not threaded into ctx-accepting callees.

```bash
sqry context-propagation .
```

For MCP, use the `context_propagation` tool when present in `tools/list`.

## Impact And Dependency Analysis

```bash
sqry impact authenticate --depth 3
sqry graph dependency-tree src/api
sqry subgraph authenticate --depth 2
```

Use impact analysis before changing public functions, route handlers, exported types, or cross-language boundaries.

## Semantic Diff

```bash
sqry diff main HEAD
```

Semantic diff compares symbol-level changes between refs. Use it to review API-impacting changes before publishing or requesting review.

## Visualization

```bash
sqry visualize "callers:authenticate" --format mermaid
sqry visualize "imports:serde" --format graphviz --output-file deps.dot
sqry visualize "callees:process" --format d2 --max-nodes 200
```

The relation query is positional. The old `--relation`/`--symbol` form is not the current CLI shape.

## Snapshot Wording

Binding-plane support was introduced historically in V9. Current releases write the current snapshot format, and the current writer format is V16. Avoid using historical format names as shorthand for current graph behavior.
