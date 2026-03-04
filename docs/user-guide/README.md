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

Note: Relation predicates in `sqry query` (`callers:`, `callees:`, `imports:`,
`exports:`) are served from the symbol index RelationStore/ImportStore today.
`sqry graph` and `sqry visualize` are backed by the unified graph snapshot.

Common status commands:

```bash
sqry graph --path . status
sqry config show --path .
```

## Language Notes

### Java Local Variable References

- Planned (pre-implementation): Java plugin will emit Reference edges for local variables + method parameters.
- Planned: unresolved explicit base types in anonymous/local classes are treated as ambiguous; local capture skipped unless base resolves to `Object` or no explicit base.
- Pattern variables supported in enumerated contexts only: if/while/for/ternary/`&&` RHS/switch guards.
- Pattern variable syntax: `instanceof` patterns (Java 16+), `switch` patterns/guards (Java 21+).
- No statement-level flow analysis; do-while pattern variables do not bind.

## Core CLI Entry Points

- `sqry index` builds the symbol index and unified graph snapshot.
- `sqry update` updates the symbol index (currently performs a full rebuild via the unified pipeline).
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
