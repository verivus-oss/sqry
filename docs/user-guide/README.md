# sqry User Guide (Unified Graph)

This guide describes the current, unified-graph workflow for sqry. The unified
graph is the source of truth for `sqry graph` and `sqry visualize` operations.
Relation predicates in `sqry query` still read from the symbol index
RelationStore/ImportStore.

Note: `sqry visualize` will auto-build a unified graph snapshot if one is
missing. Running `sqry index` first is still recommended for larger repos to
avoid repeated builds. If the graph is empty, `sqry visualize` exits with an
actionable error; if no relations are found, it warns and renders the root
context.

## Quick Start

```bash
# Build symbols + unified graph snapshot
sqry index .

# Fast symbol search (pattern-based)
sqry search "main" .

# Structured query with predicates
sqry query "kind:function AND name:test" .

# Graph analysis (unified graph)
sqry graph --path . trace-path main process_data

# Enumerate graph nodes/edges (unified graph)
sqry graph --path . --format json nodes --kind function

# Visualize relationships (unified graph)
sqry visualize "callers:main" --format mermaid --path .
```

## Release Highlights (v7.2.0)

The changes since `v7.1.5` are concentrated in graph analysis, MCP introspection,
and workspace safety:

- Graph analyses now share a single traversal engine across CLI, LSP, and MCP.
  This affects `trace-path`, `show_dependencies`, `dependency_impact`,
  `subgraph`, and graph export behavior.
- Traversal output is now more consistent:
  - node/edge truncation is applied atomically
  - path enumeration uses stable discovery order
  - leaf paths are reported when no explicit target symbol is provided
- MCP now exposes `expand_cache_status`, which lets assistants inspect Rust
  macro-expansion cache health without rebuilding or guessing.
- LSP path resolution now fails closed when a requested path escapes the active
  workspace root, including missing-path and symlink-parent escape cases.

Example MCP introspection flow:

```bash
sqry-mcp --list-tools | rg expand_cache_status
```

Example graph-analysis flow:

```bash
sqry graph trace-path main handle_error --path .
sqry graph dependency-tree module --path .
sqry impact authenticate --depth 3 --path .
```

## MCP Response Redaction

For hosted/external LLM usage through MCP, apply redaction before sending
results:

- [sqry-mcp-redaction/README.md](../../sqry-mcp-redaction/README.md)
- Use `standard` or `strict` presets for external providers.

## Index Validation

sqry validates index integrity on load. Use `--validate off|warn|fail` to
control strictness (global flag). With `--auto-rebuild`, `fail` triggers a
single rebuild attempt when corruption thresholds are exceeded.

Validation checks cover:

- Format + checksum (corruption)
- Dependency edges (dangling calls/imports/exports) with normalized name matching
- Orphaned files and duplicate IDs (hard failures when thresholds are exceeded)
- Graph cycles reported for visibility (warnings only)

Threshold tuning flags:

- `--threshold-dangling-refs` (default 0.05)
- `--threshold-orphaned-files` (default 0.20)
- `--threshold-id-gaps` (default 0.10)

Example:

```bash
sqry index --validate fail --auto-rebuild --threshold-dangling-refs 0.10 .
```

Status reporting:

```bash
sqry index --status --json .
sqry index --status --metrics-format prometheus .
```

## Plugin Cost Tiering And Index Semantics

sqry now persists the active plugin ids that built an index into the unified
graph manifest. That means later `sqry query`, `sqry watch`, `sqry diff`, and
other graph-loading paths reuse the plugin semantics that built the snapshot
instead of silently drifting to current defaults.

The default write path uses a curated fast path. Plugins marked
`high_wall_clock` are excluded by default unless you opt back in.

Examples:

```bash
# Default fast path
sqry index .

# Enable all high-cost plugins
sqry index --include-high-cost .

# Enable one plugin explicitly
sqry index --enable-plugin json .

# Disable one plugin explicitly
sqry index --disable-plugin json .
```

Notes:
- `--include-high-cost` and `--exclude-high-cost` are mutually exclusive.
- `--enable-language` and `--disable-language` remain accepted compatibility aliases for `--enable-plugin` and `--disable-plugin`.
- For older manifests without `plugin_selection`, sqry preserves legacy
  all-plugins behavior instead of silently applying the fast-path default.
- Current `high_wall_clock` plugins:
  - `json`
  - `servicenow-xml`

## Index Discovery

When running `sqry query` or `sqry search` from a subdirectory, sqry
automatically walks up the directory tree to find the nearest `.sqry-index`
file. This matches the behavior of tools like git, cargo, and npm.

### How It Works

1. sqry checks the specified directory for `.sqry-index`
2. If not found, it checks the parent directory
3. This continues up to the filesystem root (max 64 levels)
4. When found, results are automatically filtered to the original scope

### Example

```bash
# Index exists at /project/.sqry-index
cd /project/src/utils

# These all work and use the parent index:
sqry query "kind:function" .        # Results from src/utils/**
sqry query "kind:function" ../      # Results from src/**
sqry search --fuzzy "main" .        # Fuzzy results from src/utils/**
```

### Diagnostics

The query footer shows which index was used:

```
✓ Using index from /project - Query executed (50ms) - 40 symbols found (filtered to src/utils/**)
```

### File vs Directory Paths

- **Directory path**: Results filtered to `path:<dir>/**`
- **File path**: Results filtered to exact file `path:<file>`

```bash
sqry query "kind:function" ./src/main.rs  # Only main.rs functions
sqry query "kind:function" ./src/         # All src/** functions
```

## Unified Graph Basics

- **Graph storage**: `.sqry/graph/` holds the unified graph snapshot and manifest.
- **Config**: `.sqry/graph/config/config.json` controls graph limits and behavior.
- **Entry point**: graph build and load go through the unified graph pipeline.
- **Large C++ repos**: pathological single-file graph builds are bounded so one
  oversized translation unit cannot stall the entire index indefinitely.

Note: Relation predicates in `sqry query` (`callers:`, `callees:`, `imports:`,
`exports:`) are served from the symbol index RelationStore/ImportStore today.
`sqry graph` and `sqry visualize` are backed by the unified graph snapshot.

Traversal-specific notes for `7.2.0`:
- traversal-backed commands now share one limit model (`max_depth`, node caps,
  edge caps, and path caps)
- truncation metadata is produced consistently across interfaces
- path enumeration can now report root-to-leaf paths even when no target symbol
  is specified
- downstream consumers can correlate nodes and edges by stable `node_id`

Common status commands:

```bash
sqry graph --path . status
sqry config show --path .
```

## Language Notes

### Java Local Variable References

- Java plugin emits `Reference` edges for local variables and parameter bindings, including constructor, lambda, resource, catch, and compact-constructor parameters.
- Anonymous/local class resolution prefers declared and inherited members before capture; in-file bases, classpath-index bases, and seeded well-known JDK bases resolve before capture, while truly unknown external bases remain ambiguous.
- Pattern variables supported in enumerated contexts: if/while/for/ternary/`&&` RHS/switch guards.
- Pattern variable syntax: `instanceof` patterns (Java 16+), `switch` patterns/guards (Java 21+).
- Statement-level flow is supported after `if`, `while`, `do`, and `for` when Java's definite-match rules guarantee continuation implies a successful pattern match; do-while pattern variables still do not bind inside the loop body.

## Core CLI Entry Points

- `sqry index` builds the symbol index and unified graph snapshot.
- `sqry update` updates the symbol index (currently performs a full rebuild via the unified pipeline).
- `sqry index` and `sqry update` accept plugin-selection flags for fast-path and
  high-cost plugin control.
- `sqry search` performs pattern-based symbol search.
- `sqry query` runs AST-aware predicate queries with boolean logic (relation
  predicates use the index RelationStore/ImportStore).
- `sqry graph` runs graph analyses (trace paths, stats, cycles, complexity).
- `sqry diff` compares semantic symbol changes between two git refs.
- `sqry visualize` renders relation queries to diagram formats from the unified
  graph snapshot.
- `sqry ask` translates natural language into safe sqry commands.
  See [natural-language.md](natural-language.md) for the full guide.
- `sqry shell` starts an interactive session with a warm cache.
- `sqry batch` executes multiple queries from a batch file.
- `sqry config` manages unified graph config and aliases.
- `sqry cache` manages the persisted AST cache.
- `sqry lsp` starts the language server.

## Workspace (Multi-Repo) Commands

Manage multiple repositories under a single workspace root (CLI):

```bash
sqry workspace init /projects --name "My Workspace"
sqry workspace scan /projects --mode git-roots
sqry workspace add /projects /projects/backend --name backend
sqry workspace remove /projects backend
sqry workspace query /projects "kind:function AND repo:backend"
sqry workspace stats /projects
```

Discovery modes:
- `index-files` (default): find `.sqry-index` files under the root
- `git-roots`: require a `.git/` directory plus `.sqry-index`

## Advanced Analysis Commands

Standalone analyses powered by the unified graph:

```bash
sqry duplicates --type body --threshold 90
sqry cycles --type imports --min-depth 3
sqry unused --scope public --lang rust
sqry explain src/main.rs main
sqry similar src/lib.rs process_data --threshold 0.8
sqry subgraph main --depth 3 --include-imports
sqry impact authenticate --depth 5 --show-files
sqry diff main HEAD --change-type signature_changed
sqry hier "kind:function AND name:parse" --max-files 10 --context 5
sqry export --format mermaid --filter-lang rust,go --output graph.mmd
```

Behavior notes:
- `sqry impact` defaults still matter: keep `--depth` small unless you want
  transitive blast-radius analysis.
- `sqry graph trace-path` and related consumers now share the same traversal
  semantics as MCP/LSP, so truncation and path ordering should match across
  interfaces more closely than in earlier releases.

## Semantic Diff (Git Refs)

Use `sqry diff` to compare symbol-level changes between two refs (commit, branch,
or tag). This is useful for release notes, API review, and refactor validation.

```bash
# Compare branches
sqry diff main feature/auth-refactor

# Focus on function signature changes only
sqry diff v1.0.0 v2.0.0 --kind function --change-type signature_changed

# Limit output and emit JSON for automation
sqry diff HEAD~20 HEAD --limit 200 --json

# Run from a subdirectory by pointing at repo path explicitly
sqry diff main HEAD --path .
```

Supported `--change-type` values: `added`, `removed`, `modified`, `renamed`,
`signature_changed`.

## Aliases, History, and Diagnostics

```bash
sqry query "kind:function AND name:test" --save-as test-funcs
sqry alias list --global
sqry alias export aliases.json
sqry history list --limit 50
sqry insights show
sqry troubleshoot --dry-run
```

Notes:
- Aliases are stored locally (project) or globally (`~/.config/sqry/`).
- History entries redact common secrets.
- Insights are local-only; no network calls are made.

## Output UX Options

```bash
sqry --pager query "kind:function"
sqry --no-pager search "error" --json
sqry --pager-cmd "less -R" query "name:main"
sqry --theme light --sort name query "kind:function"
```

## LSP Custom Endpoints & Configuration

`sqry lsp` exposes standard LSP handlers plus custom `sqry/*` endpoints for
semantic search, relations, graph export, and analysis. For a full endpoint
list and configuration keys, see `docs/FEATURE_LIST.md`.

## Graph Operations

`sqry graph` supports these unified graph analyses:

- `trace-path <from> <to>`: shortest path across call/edge types.
- `call-chain-depth <symbol>`: maximum call depth (optional chain output).
- `dependency-tree <module>`: transitive import dependency tree.
- `cross-language`: list cross-language edges with filters.
- `stats`: node/edge summary statistics.
- `status`: unified graph snapshot status.
- `cycles`: circular dependency detection.
- `complexity`: complexity metrics for symbols or modules.
- `nodes`: list unified graph nodes with filters.
- `edges`: list unified graph edges with filters.

Example:

```bash
sqry graph --path . --format json stats
```

Node/edge listing examples:

```bash
sqry graph --path . --format json nodes --kind macro --languages rust
sqry graph --path . --format json edges --kind http_request --from-lang rust
```

Filtering semantics:

- `--kind` and language filters are case-insensitive exact matches.
- `--name`, `--qualified-name`, `--from`, `--to` are case-sensitive substrings.
- `--file` is a case-insensitive substring on normalized paths (edges use the
  source/edge file).

## Output Formats

Global output flags apply to most commands:

- `--json`, `--csv`, `--tsv` for structured output
- `--headers` and `--columns` for CSV/TSV control
- `--preview` for code context around matches
- `--sort` for stable, opt-in sorting

## Suggested Workflow

1. Run `sqry index` to build the symbol index and unified graph snapshot.
2. Use `sqry search` or `sqry query` to find symbols of interest.
3. Use `sqry graph` to analyze relationships and call paths (unified graph).
4. Use `sqry visualize` to export diagrams (unified graph).

For diagram formats and rendering options, see `docs/user-guide/visualization.md`.
