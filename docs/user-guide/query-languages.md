# Query Languages

sqry has two text query surfaces. They are not predicate-equivalent. Pick one and stay on it for a given command.

| Surface | Commands | Engine |
| --- | --- | --- |
| Core query parser | `sqry query`, MCP `semantic_search` / `hierarchical_search` `query` strings | `sqry-core` query parser |
| Planner | `sqry plan-query`, MCP `sqry_query` | `sqry-db` planner |

`sqry search` is a third surface: regex or `--exact` literal name matching. It does not accept `kind:function` or `AND` / `OR`.

## Core query parser (`sqry query`)

Accepts boolean `AND` / `OR` / `NOT`, `name~=/regex/`, and relation predicates such as `callers:`, `callees:`, `imports:`, `exports:`, `returns:`.

```bash
sqry query "kind:function AND visibility:public AND lang:rust"
sqry query "kind:function AND name~=/_set$/ "
sqry query "kind:function AND returns:Result"
```

On workspaces with more than about 50k symbols, pair every `name~=/regex/` with `lang:`, `path:`, or `kind:` or the cost gate returns `query_too_broad`.

`name:<literal>` on this surface is a contains-style match unless you use the planner or `sqry search --exact`. For byte-exact name lookup use `sqry search --exact authenticate` or the planner `name:` predicate.

## Planner (`sqry plan-query`)

Accepts a predicate chain without boolean `AND` tokens (whitespace is conjunction). It does **not** accept `name~=`.

```bash
sqry plan-query "kind:function callers:main"
sqry plan-query "kind:function callers:my_read resolved_via:binding_plane"
sqry plan-query "kind:function cfg:linux"
sqry plan-query "kind:function wraps:errors_is"
sqry plan-query "kind:function shape~=parse_config"
sqry plan-query "kind:function is_definition:true"
```

Planner-only or planner-first predicates:

- `cfg:<flag>` / `cfg:"<expr>"` for stored cfg metadata
- `wraps` / `wraps:<wrap-kind>` for Go `Wraps` error-chain edges
- `shape~=<symbol>` for identifier-blind body-shape neighbours
- `address_taken` / `callsite_promiscuous` for C indirect-call precision
- `resolved_via:<kind>` with the eight live values: `direct`, `type_match`, `binding_plane`, `virtual_dispatch`, `interface_dispatch`, `duck_typed`, `structural`, `promiscuous_elided`
- `items:true` / `is_definition:true` for definition-only filtering (also accepted on some query/search tools)

`framework:<id>` parses in the planner. Framework-route metadata is populated only where extractors have run; treat a non-matching filter as absent metadata, not proof that a framework is unused. MCP tools also expose a typed `framework` parameter.

## Which command to use

- Name regex, boolean logic, or `returns:` from an interactive session: `sqry query`.
- Joins, `cfg:`, `wraps:`, `shape~=`, or MCP-authored structural IR: `sqry plan-query` / `sqry_query`.
- One literal or regex name with `--kind` / `--lang`: `sqry search`.

See [Advanced Analysis](advanced-analysis.md) for `returns:`, `resolved_via`, and definition-only behaviour, and [`docs/cli/query.md`](../cli/query.md) for planner predicate details.
