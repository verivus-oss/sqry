---
name: sqry-semantic-search
version: 33.0.1
description: |
  AST-based semantic code search skill for AI agents. Teaches agents to use sqry MCP resources when connected and the sqry CLI when MCP is unavailable. sqry parses code like a compiler using ASTs and graph queries, not embeddings.
---

# sqry Semantic Code Search Skill

**Routing rule:** For structural code search (symbols, callers, callees, impact, dependencies), use sqry MCP tools when connected; otherwise use the `sqry` CLI from the workspace root. Use `rg`/grep only for literal text, and native file tools only for reading full files.

Use this skill when users ask to:

- find functions, classes, methods, variables, types, modules, or files by code structure;
- trace callers, callees, references, imports, inheritance, or call paths;
- analyze dependency impact, unused symbols, cycles, duplicates, or semantic diffs;
- understand a codebase through AST-backed graph facts instead of text guessing.

## What Makes sqry Different

sqry uses "semantic" in the compiler sense. It parses code into ASTs, builds a graph of symbols and relationships, and answers structural queries from that graph. It is not an embedding search tool.

## Current Version Target

This skill targets public `verivus-oss/sqry` v33.0.1. Its version numbers advance with each release; the figures below are maintained by hand, and the installed runtime is the source of truth (see below):

- 37 languages: 28 full-relation languages and 9 symbol-extraction languages. The released binaries compile 30 of them (29 enabled by default, JSON with `--include-high-cost`); `pulumi`, `puppet`, `salesforce-apex`, `sap-abap`, `servicenow-xanadu`, `servicenow-xml` and `terraform` are cargo features and a release binary rejects `--enable-plugin` for them
- 39 MCP tools standalone, 17-tool subset when daemon-hosted (see the tool table below)
- 6 MCP resources and 6 MCP prompts standalone, none when daemon-hosted
- snapshot format 17 (`sqry://meta/manifest` `snapshot_format`; `snapshot.sqry` starts with `SQRY_GRAPH_V17`; distinct from the on-disk `.sqry/graph/manifest.json` `snapshot_format_version`)
- default MCP redaction preset: `minimal`
- default query timeout: 60s (`SQRY_MCP_TIMEOUT_MS=60000`)
- default index timeout: 600s (`SQRY_MCP_INDEX_TIMEOUT_MS=600000`)
- MCP protocol 2024-11-05 over stdio
- Rust 1.94+, Edition 2024

Use the installed MCP manifest as the runtime source of truth. `sqry --list-languages` lists only the enabled plugins (29 by default, 30 with `SQRY_INCLUDE_HIGH_COST=1`); the MCP manifest reports the language/tool surface of the source tree.

Always confirm the installed runtime:

```bash
sqry --version
sqry-mcp --version
sqry-lsp --version
sqryd --version
sqry index --status --json .
```

Regenerate pinned version, tool count, language count and snapshot format lines in this repo after upgrading sqry:

```bash
./scripts/sync-versions.sh
```

## Setup

Install or upgrade sqry:

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
sqry --version
sqry-mcp --version
sqry-lsp --version
sqryd --version
```

Build the workspace index from the project root:

```bash
sqry index .
sqry index --status --json .
```

Configure MCP for agents with first-class sqry setup support (`--tool` accepts `claude`, `codex`, `gemini`, or `all`, the default):

```bash
sqry mcp setup --tool claude
sqry mcp setup --tool codex
sqry mcp setup --tool gemini
sqry mcp status
```

Every entry `sqry mcp setup` writes carries `args = ["--no-daemon"]`, so a configured host lands on the full standalone surface. `--dry-run` previews the exact entry; `--scope project|global` and `--workspace-root <path>` apply to Claude Code only.

After MCP setup, restart the agent. The current session usually cannot reload newly configured MCP servers.

### MCP mode: standalone vs daemon

**Default for manifest/docs workflows:** use standalone `sqry-mcp --no-daemon`. Standalone serves **39 tools**, **6 MCP resources** (`sqry://meta/manifest` and `sqry://docs/*`) and **6 prompts**.

**Daemon mode** (`sqry-mcp --daemon`) is for long sessions with a warm graph, but it exposes only a **17-tool subset**, **zero MCP resources** and **zero prompts**: you cannot read `sqry://meta/manifest` or other docs on the daemon path. Do not recommend daemon then instruct reading MCP resources in the same workflow.

**No flag at all** makes `sqry-mcp` probe for a running `sqryd` and connect to it when one is reachable, falling back to standalone otherwise. An unflagged entry can therefore land on the 17-tool subset without warning; pin `--no-daemon` when the full surface matters.

```bash
# Standalone: full tool surface + resources + prompts (preferred for docs/manifest)
sqry-mcp --no-daemon

# Daemon: warm graph, 17-tool subset, no resources, no prompts
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Plugin `.mcp.json` uses standalone by default. Add `"args": ["--daemon"]` only when you accept the 17-tool, no-resource tradeoff.

## Redaction for external LLMs

MCP responses are redacted by default (`SQRY_REDACTION_PRESET=minimal`). For cloud or untrusted agents, set the preset before launching `sqry-mcp`:

```bash
# Documented on sqry-mcp --help:
# SQRY_REDACTION_PRESET=none|minimal|relative|standard|strict (default: minimal)

export SQRY_REDACTION_PRESET=standard   # cloud LLMs, code confidential
# export SQRY_REDACTION_PRESET=strict  # untrusted external services
sqry-mcp --no-daemon
```

The preset applies to a standalone `sqry-mcp` only. In shim mode (`--daemon`, or the default whenever a daemon is reachable) the daemon redacts under the preset in **its own** environment and the client's setting is ignored: set `SQRY_REDACTION_PRESET` where `sqryd` runs (its service unit or launching shell, then restart it), or keep `--no-daemon` as above.

Preset semantics (see the `sqry-mcp-redaction` crate README in the sqry repo): `relative` rewrites absolute paths to workspace-relative ones; `standard` for cloud LLMs when code must stay confidential; `strict` for untrusted externals. `sqry://docs/tool-guide` documents field-level semantics but does **not** replace the preset table; cite `sqry-mcp --help` and the redaction README for external-LLM guidance.

## Provenance filters (C today; do not oversell)

The live `semantic_search` description documents three C-scoped predicates (`address_taken:true|false`, `resolved_via:direct|type_match|binding_plane`, `callsite_promiscuous:true|false`) populated by the **C plugin only**; on non-C nodes they evaluate to false.

`direct_callers`, `direct_callees`, `relation_query`, `semantic_search` and `sqry_query` also accept two optional filter parameters: `framework` (one of `asp_net_core`, `actix`, `axum`, `chi`, `django`, `express`, `fast_api`, `fastify`, `flask`, `gin`, `koa`, `laravel`, `nest_js`, `rails`, `rocket`, `sinatra`, `spring`, `starlette`, `symfony`) and `resolved_via` (an array drawn from `direct`, `type_match`, `binding_plane`, `virtual_dispatch`, `interface_dispatch`, `duck_typed`, `structural`, `promiscuous_elided`). So far only the C plugin emits provenance, and no language plugin writes the framework-route or dispatch tables those filters read, so `framework` and the five non-C `resolved_via` members match nothing yet. The parameters are accepted, not rejected.

Do not headline `binding_plane` or `resolved_via` filters as a universal capability on Python/TypeScript/Rust repos until resolver support ships. Prefer reading the live tool description via `sqry://docs/tool-guide` after connecting standalone MCP.

## Use MCP When Connected

First confirm the sqry MCP server is visible to the current agent. On **standalone** MCP, read:

```text
sqry://meta/manifest
```

Then use the live resources for the installed version:

| I need to... | Read this MCP resource |
|-------------|------------------------|
| Find the right tool | `sqry://docs/capability-map` |
| See tool parameters | `sqry://docs/tool-guide` |
| Write a query | `sqry://docs/query-syntax` |
| Follow a workflow recipe | `sqry://docs/patterns` |
| Understand graph internals | `sqry://docs/architecture` |
| Check version and counts | `sqry://meta/manifest` |

MCP tool names can be host-specific. Claude Code, Codex, and Gemini commonly expose prefixed names such as `mcp__sqry__semantic_search`; other hosts may expose canonical names such as `semantic_search`.

### Tool reference (v33.0.1, standalone `sqry-mcp --no-daemon`)

Required arguments come from the live `tools/list` schema. Every tool also accepts optional `path` (workspace root, default `.`). Where a tool lists `max_results` it is optional with the default shown. "Daemon" marks the 17 tools that a daemon-hosted connection also serves.

| Tool | Required arguments | Daemon | Purpose |
|------|--------------------|--------|---------|
| `semantic_search` | `query` | yes | Search symbols by name, kind, visibility, language (`max_results` 200; `filters` object with `language`, `symbol_kind`, `visibility`, `score_min`, `cfg_condition`) |
| `hierarchical_search` | `query` | no | Search with results grouped by file and container for RAG (`max_results` 200, `max_files` 20) |
| `pattern_search` | `pattern` | no | Substring match on symbol names (`max_results` 100) |
| `get_workspace_symbols` | `query` | no | Symbol name search across the workspace (`max_results` 100) |
| `sqry_query` | `query` | no | Planner text query, a whitespace-separated predicate chain such as `kind:function has:caller` or `kind:function callers:main` (`limit` optional) |
| `get_definition` | `symbol` | no | Where a symbol is defined |
| `get_hover_info` | `symbol` | no | Signature, documentation, type info |
| `get_references` | `symbol` | no | All references (`include_declaration` true, `max_results` 100) |
| `explain_code` | `file_path`, `symbol_name` | no | Explain one symbol (`include_context` true, `include_relations` true) |
| `get_document_symbols` | `file_path` | no | All symbols defined in one file |
| `direct_callers` | `symbol` | yes | Immediate callers, depth 1 (`max_results` 100) |
| `direct_callees` | `symbol` | yes | Immediate callees, depth 1 (`max_results` 100) |
| `call_hierarchy` | `symbol`, `direction` (`incoming` or `outgoing`) | no | Call tree (`max_depth` 1, `max_results` 200) |
| `relation_query` | `symbol`, `relation_type` (`callers`, `callees`, `imports`, `exports`, `returns`) | yes | One relation kind for a symbol (`max_depth` 1, `max_results` 200) |
| `trace_path` | `from_symbol`, `to_symbol` | yes | Ranked call paths between two symbols (`max_hops` 5, `max_paths` 5, `min_confidence` 0.5) |
| `dependency_impact` | `symbol` | yes | What breaks if a symbol changes (`max_depth` 3, `include_indirect` true, `max_results` 500) |
| `show_dependencies` | none (`symbol_name` or `file_path` selects the root) | yes | Dependency tree for a file or symbol (`max_depth` 2) |
| `subgraph` | `symbols` (array) | yes | Focused subgraph around seed symbols (`max_depth` 2, `max_nodes` 50) |
| `export_graph` | none | yes | Export a subgraph; `format` is `json`, `dot`, `d2` or `mermaid` (`max_depth` 2) |
| `cross_language_edges` | none | no | Edges whose caller and callee languages differ (`from_lang`, `to_lang` optional) |
| `find_unused` | none | yes | Unreachable or unused symbols; `scope` is `public`, `private`, `function`, `struct` or `all` (`max_results` 100) |
| `find_cycles` | none | yes | Cycles; `cycle_type` is `calls`, `imports` or `modules` (`min_depth` 2, `max_results` 100) |
| `is_node_in_cycle` | `symbol` | yes | Whether one symbol sits in a cycle (`cycle_type` as above) |
| `find_duplicates` | none | no | Duplicates; `duplicate_type` is `body`, `signature` or `struct` (`threshold` 80, `exact` false) |
| `search_similar` | `reference` (object with a `file_path` and a `symbol_name`) | no | Fuzzy-similar symbols (`similarity_threshold` 0.7, `max_results` 20) |
| `structural_similar` | `symbol_name` | yes | Structurally similar functions via the body-shape descriptor (`similarity_threshold` 0.7) |
| `semantic_diff` | `base`, `target` (each an object with a git `ref` string) | yes | Symbol-level changes between two git refs (`filters.change_types` from `added`, `removed`, `modified`, `renamed`, `signature_changed`) |
| `complexity_metrics` | none | yes | Complexity estimate per function (`min_complexity` 1, `max_results` 100) |
| `get_insights` | none | no | Codebase health metrics |
| `generate_overview` | none | yes | One-call repository orientation map (`top` 10, `group_depth` 2, `sections` optional) |
| `context_propagation` | none | no | Go `context.Context` leaks; `mode` is `all`, `break_site`, `unthreaded_goroutine` or `http_handler_leak` |
| `rules_run` | `rule_or_pack` | no | Run a declarative rule or pack (for example `bbnty.all`) |
| `list_symbols` | none | no | Indexed symbols filtered by `kind` and `language` (`max_results` 500; `summary` true for counts only) |
| `list_files` | none | no | Indexed files, optionally by `language` (`max_results` 500) |
| `get_graph_stats` | none | no | Node, edge, file counts and language breakdown |
| `get_index_status` | none | no | Index status and metadata |
| `workspace_status` | none | no | Aggregate index status of the logical workspace (`workspace_id` optional) |
| `expand_cache_status` | none | no | Macro expansion cache status (`.sqry/expand-cache/`) |
| `rebuild_index` | none | yes | Rebuild the graph from source (`force` true) |

Common optional shapes: `pagination` is an object `{"cursor": "<opaque>", "page_size": <1..500>}`; tools without `pagination` use `page_size` and `page_token` at the top level.

Example invocations (argument names as the schema spells them; all read-only):

```text
semantic_search {"query": "authenticate", "max_results": 20}
semantic_search {"query": "handle", "filters": {"symbol_kind": ["function"], "language": ["rust"]}}
sqry_query {"query": "kind:function callers:main", "limit": 50}
get_definition {"symbol": "AuthService::authenticate"}
get_references {"symbol": "handle_request", "include_declaration": false}
direct_callers {"symbol": "AuthService::authenticate", "max_results": 50}
relation_query {"symbol": "main", "relation_type": "callees"}
call_hierarchy {"symbol": "main", "direction": "outgoing", "max_depth": 2}
trace_path {"from_symbol": "main", "to_symbol": "handle_request", "max_hops": 4}
dependency_impact {"symbol": "AuthService::authenticate", "max_depth": 2}
find_unused {"scope": "all", "max_results": 50}
find_cycles {"cycle_type": "calls"}
semantic_diff {"base": {"ref": "HEAD~1"}, "target": {"ref": "HEAD"}}
explain_code {"file_path": "src/api.ts", "symbol_name": "execute"}
generate_overview {"top": 5}
```

## Quick Tool Selection

The table above maps a tool to its arguments. This maps a task to a tool.

**I know the symbol name and want to...**

- see its definition: `get_definition`
- see its signature or docs: `get_hover_info`
- see all references: `get_references`
- see who calls it: `direct_callers` (depth 1) or `relation_query` (multi-depth)
- see what it calls: `direct_callees` (depth 1) or `relation_query` (multi-depth)
- see the call tree: `call_hierarchy` (set `max_depth` above 1 for deeper traversal)
- see its context and source: `explain_code`
- see what breaks if I change it: `dependency_impact`
- check whether it sits in a cycle: `is_node_in_cycle`
- find similar symbols by name or signature: `search_similar`
- find structurally similar bodies, rename-blind: `structural_similar`

**I want to search for symbols...**

- by name substring: `pattern_search`
- by name with ranking: `get_workspace_symbols`
- by kind, visibility or language: `semantic_search`
- by structural predicates (`has:caller`, `unused`, `items`, `shape~=`, `wraps`, `resolved_via`): `sqry_query`
- with results grouped for RAG context: `hierarchical_search`

**I want to analyse the codebase...**

- circular dependencies: `find_cycles`
- dead code: `find_unused`
- exact duplicate code: `find_duplicates`
- rename-and-relocate twins: `structural_similar`
- compare git versions: `semantic_diff`
- run declarative rule packs: `rules_run`
- Go context propagation: `context_propagation`
- overall stats: `get_graph_stats`
- health metrics: `get_insights`
- complexity scores: `complexity_metrics`

**I want to visualise or export...**

- dependency tree: `show_dependencies`
- subgraph around symbols: `subgraph`
- graph as DOT, Mermaid or D2: `export_graph`
- call path between two symbols: `trace_path`
- cross-language edges: `cross_language_edges`

**I want to list everything...**

- indexed files: `list_files`
- indexed symbols: `list_symbols`
- repo-scale overview without blowing the budget: `list_symbols` with `summary: true`
- symbols in one file: `get_document_symbols`
- workspace resolution and graph state: `workspace_status`

## Understand Before Changing

Work broad, then narrow, then impact.

1. **Big picture.** `get_graph_stats` for node and edge counts and the language breakdown, `get_insights` for cycles, unused symbols and duplicates, `list_files` for what is indexed.
2. **Find relevant symbols.** `semantic_search` by structure (`kind:function name:*auth*`), `pattern_search` for a substring, `get_workspace_symbols` for ranked fuzzy names, `hierarchical_search` when the result feeds RAG context.
3. **Understand one symbol.** `get_definition` for where, `get_hover_info` for signature and type, `explain_code` for context and relations, `get_document_symbols` for what shares the file.
4. **Trace before changing.** `direct_callers` and `direct_callees` at depth 1, `relation_query` deeper, `dependency_impact` for blast radius, `subgraph` for the surrounding context graph.
5. **Check quality.** `find_cycles` and `is_node_in_cycle` near the change, `find_unused` for dead code around it.

Changing one function, end to end: `get_definition` to find it, `explain_code` to understand it, `dependency_impact` for what breaks, `direct_callers` for who depends on it, `get_document_symbols` for the rest of the file. After committing, `semantic_diff` between the base ref and HEAD to verify the symbol-level delta.

## Use CLI When MCP Is Missing

If MCP tools or resources are not visible, do not stop. Use the CLI from the workspace root. Put flags before the positional query; predicates such as `kind:`, `name:`, `path:` and `lang:` belong to `sqry query` (the bare `sqry <pattern>` form searches symbol names and does not evaluate predicates):

```bash
# Find symbols with structured query syntax
sqry query --json 'kind:function AND name:authenticate'
sqry query --json 'kind:class AND lang:typescript'
sqry query --json --limit 50 'kind:method AND path:src/auth'

# Fast name search flags for common cases
sqry --json --kind function --exact authenticate
sqry --json --kind class 'Manager|Executor|Recorder'

# Graph relationships
sqry graph direct-callers --json "AuthService::authenticate"
sqry graph direct-callees --json "main"
sqry graph trace-path --json "main" "handle_request"

# Analysis workflows
sqry impact --json "AuthService::authenticate"
sqry unused --json
sqry cycles --json
sqry duplicates --json
sqry diff --json HEAD~1 HEAD

# Planner query (same syntax as the sqry_query MCP tool)
sqry plan-query 'kind:function callers:main'
```

There is no natural-language command: `sqry ask` and the `sqry_ask` MCP tool were removed in sqry v21. Translate the question into one of the structured commands above.

Use CLI fallback especially when:

- the current agent has no sqry MCP server connected;
- `sqry://meta/manifest` cannot be read (often because MCP is daemon-backed with no resources);
- MCP discovery only shows unrelated servers;
- the agent supports shell commands but not MCP tools;
- the user needs immediate recovery before restarting the agent.

## Workspace-Aware Usage

sqry can resolve logical workspaces from a `.sqry-workspace` registry or a VS Code `.code-workspace` file containing `sqry.workspace`.

When working in a multi-root session:

1. Prefer explicit `path` or file-bearing arguments when the target source root is ambiguous.
2. Use `sqry workspace status --json --no-cache <workspace>` outside MCP to inspect source-root health.
3. For MCP calls, rely on session-scoped resolution in this order: explicit `path`, file-bearing arguments, MCP roots, last-resolved workspace, then legacy environment/CWD fallback.

## Handling Ambiguous Symbols

Names such as `new`, `init`, `handle`, `process`, `execute`, and `request` often exist in many files.

Use one or more of:

- path filters: `sqry query --json 'kind:function AND name:handle AND path:src/api'`
- exact matching: `sqry --json --kind function --exact handle`
- qualified names: `UserService::authenticate`
- a search-first workflow: find the symbol, then run callers/callees/impact with the qualified name

Rule of thumb: if a name could exist in more than one file, add a `path:` filter or use a qualified name. `sqry explain <FILE> <SYMBOL>` refuses ambiguous names and lists the candidates; pass `--line <N>` to pick one.

## Scoping For Large Indexes

On any workspace whose graph reports more than roughly 50,000 nodes (`get_graph_stats`), an unbounded regex over symbol names is the single most common cause of MCP timeouts and transport-closed errors.

**Pairing rule:** every `name~=/regex/` predicate must be combined with at least one of `lang:`, `path:` or `kind:`.

```text
# Too broad on a 250k-node workspace
name~=/.*_set$/

# Scoped
kind:method name~=/.*_set$/
lang:rust name~=/.*_set$/
path:src/api/ name~=/.*_set$/
```

The rule applies to every search tool in the reference above: `semantic_search`, `hierarchical_search`, `pattern_search` and `get_workspace_symbols`. The cost gate enforces it and returns `query_too_broad` rather than hanging, so an unpaired regex fails fast instead of timing out.

## Output Size Tips

Start narrow, then expand:

- use `direct_callers` / `direct_callees` before `call_hierarchy` or `dependency_impact`;
- set `max_results`, `max_depth`, `max_nodes` or `page_size` explicitly (the defaults are in the table above);
- add `path:`, `kind:`, and `lang:` filters early;
- prefer `sqry explain <FILE> <SYMBOL>`, `get_hover_info` or `get_definition` for quick lookups.

MCP responses are truncated at 50,000 bytes by default. Override with `SQRY_MCP_MAX_OUTPUT_BYTES=<n>` only when the transport can handle larger payloads. CLI output is not truncated by MCP.

## Rebuild and Upgrade Recovery

After upgrading sqry across graph semantics changes:

```bash
sqry index --force .
sqry index --status --json .
```

If graph loading fails with stale format or unknown plugin IDs:

```bash
rm -rf .sqry/graph .sqry/graphs .sqry/analysis
sqry index --force .
```

If daemon-backed results disagree with persisted artifacts or relation queries time out, cross-check with:

```bash
sqry index --force .
sqry index --status --json .
sqry query --json '<narrow structural query>'
```

## When Not To Use sqry

- Literal text search: use `rg` or the agent's grep tool.
- File name discovery: use file listing, glob, or find tools.
- Reading full source files: use the agent's file read tool.
- Running code: sqry searches and analyzes code; it does not execute it.

Hybrid workflow: use text search for exact strings, then use sqry to understand symbols, relations, and impact.

## Troubleshooting

- No MCP tools visible: configure MCP, restart the agent, or use CLI fallback.
- Cannot read `sqry://meta/manifest`: switch to standalone `sqry-mcp --no-daemon` (daemon serves zero resources).
- Fewer than 39 tools listed: the connection is daemon-hosted (17-tool subset). Pin `--no-daemon` for the full surface.
- Empty results: run `sqry index .` from the correct workspace root, or `sqry index --force .` after an upgrade or stale graph warning.
- Unknown plugin IDs: remove `.sqry/graph`, `.sqry/graphs`, and `.sqry/analysis`, then rebuild. If the message names a plugin the binary does not support, the index was built by a differently featured binary; rebuild with the installed one.
- Cost gate rejection: narrow the query with `kind:`, `lang:`, `path:`, or a more specific name.
- Large relation query times out: reduce depth or scope, then expand.
- Missing high-cost symbols: only when the user asks for JSON support, rebuild with `sqry index --include-high-cost .`.
