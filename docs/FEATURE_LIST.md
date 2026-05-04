# sqry Feature List

This document provides a comprehensive list of all features available across sqry's interfaces.

## CLI Commands (`sqry`)

### Core Commands

| Command | Description |
|---------|-------------|
| `sqry index` | Build or update the semantic index for a workspace |
| `sqry search <query>` | Semantic code search using natural language or structured queries |
| `sqry query <expression>` | Execute structured queries against the symbol index |
| `sqry ask <question>` | Natural language queries with AI-powered responses |
| `sqry graph <subcommand>` | Graph operations (nodes, edges, stats, etc.) |

### Graph Subcommands

| Subcommand | Description |
|------------|-------------|
| `sqry graph nodes` | List nodes in the code graph |
| `sqry graph edges` | List edges (relationships) in the code graph |
| `sqry graph stats` | Show graph statistics |
| `sqry graph status` | Show graph build status |
| `sqry graph cycles` | Find circular dependencies in the graph |
| `sqry graph trace-path` | Find shortest path between two symbols |
| `sqry graph call-chain-depth` | Calculate maximum call depth from a symbol |
| `sqry graph dependency-tree` | Show transitive dependencies for a symbol |
| `sqry graph cross-language` | List cross-language relationships |
| `sqry graph complexity` | Calculate code complexity metrics |
| `sqry graph direct-callers` | Find direct callers of a symbol |
| `sqry graph direct-callees` | Find direct callees of a symbol |
| `sqry graph call-hierarchy` | Show call hierarchy tree |
| `sqry graph is-in-cycle` | Check if a symbol is part of a cycle |

### Index Management

| Command | Description |
|---------|-------------|
| `sqry index` | Build/rebuild the index |
| `sqry index --status` | Show index status and statistics |
| `sqry update` | Incrementally update the index |
| `sqry watch` | Watch for file changes and update index in real-time |
| `sqry repair` | Repair corrupted index files |

Indexing behavior highlights:
- `sqry index` / `sqry update` support `--include-high-cost`, `--exclude-high-cost`, `--enable-plugin`, `--disable-plugin`
- active plugin ids are persisted in the unified graph manifest
- read-only indexed commands reuse manifest-backed plugin selection by default
- pathological single-file C++ graph builds are bounded for large repositories

### Cache Management

| Command | Description |
|---------|-------------|
| `sqry cache stats` | Show cache statistics |
| `sqry cache clear` | Clear the cache |
| `sqry cache prune` | Remove stale cache entries |

### Configuration

| Command | Description |
|---------|-------------|
| `sqry config init` | Initialize configuration file |
| `sqry config show` | Display current configuration |
| `sqry config get <key>` | Get a configuration value |
| `sqry config set <key> <value>` | Set a configuration value |
| `sqry config validate` | Validate configuration file |
| `sqry config alias list` | List query aliases |
| `sqry config alias set` | Create a query alias |
| `sqry config alias remove` | Remove a query alias |

### Workspace Management

| Command | Description |
|---------|-------------|
| `sqry workspace` | Show workspace information |
| `sqry workspace init` | Initialize a new workspace |
| `sqry workspace scan` | Scan workspace for files |
| `sqry workspace add` | Add files/directories to workspace |
| `sqry workspace remove` | Remove files/directories from workspace |
| `sqry workspace query` | Query workspace configuration |
| `sqry workspace stats` | Show workspace statistics |

### Analysis & Insights

| Command | Description |
|---------|-------------|
| `sqry insights` | Generate codebase insights and metrics |
| `sqry insights show` | Display current insights |
| `sqry insights config` | Configure insight generation |
| `sqry insights status` | Show insights generation status |
| `sqry insights prune` | Remove stale insights |
| `sqry visualize` | Generate visual representations of code structure |
| `sqry troubleshoot` | Diagnose common issues |
| `sqry duplicates` | Find duplicate code (body, signature, struct) |
| `sqry cycles` | Find circular dependencies (calls, imports, modules) |
| `sqry unused` | Find unused/dead code via reachability analysis |
| `sqry export` | Export code graph (DOT, D2, Mermaid, JSON) |
| `sqry diff` | Semantic diff between git refs (compares symbol-level changes) |
| `sqry explain` | Explain a symbol with context and relationships |
| `sqry similar` | Find symbols similar to a reference |
| `sqry subgraph` | Extract focused subgraph around symbols |
| `sqry impact` | Analyze what would break if a symbol changes |
| `sqry hier` | Hierarchical search optimized for RAG retrieval |

### Utilities

| Command | Description |
|---------|-------------|
| `sqry shell` | Interactive REPL for queries |
| `sqry batch` | Execute multiple queries from a file |
| `sqry history` | View query history |
| `sqry history search` | Search query history |
| `sqry history clear` | Clear query history |
| `sqry history stats` | Show history statistics |
| `sqry completions` | Generate shell completions (bash, zsh, fish, powershell) |
| `sqry alias` | Manage command aliases |
| `sqry alias show` | Show alias details |
| `sqry alias delete` | Delete an alias |
| `sqry alias rename` | Rename an alias |
| `sqry alias export` | Export aliases to file |
| `sqry alias import` | Import aliases from file |

---

## Release Highlights (v7.2.0)

- Shared traversal kernel across CLI, LSP, and MCP graph analysis operations
- Deterministic path enumeration with atomic node/edge truncation and leaf-path reporting
- `sqry-bind` facade plus reusable traversal/materialization result types in `sqry-core`
- MCP `expand_cache_status` introspection tool and dynamic assistant-facing tool docs
- LSP workspace-boundary hardening for path resolution

## MCP Tools (Model Context Protocol)

sqry provides **36 MCP tools** for AI/LLM integration:

### MCP Response Redaction

- **Library**: `sqry-mcp-redaction`
- **Purpose**: redact paths, workspace roots, and optional code/docs fields before MCP responses are sent to external LLM services
- **Docs**: [`sqry-mcp-redaction/README.md`](../sqry-mcp-redaction/README.md)

### Semantic Search

| Tool | Description |
|------|-------------|
| `semantic_search` | Advanced semantic code search with filters and pagination |
| `hierarchical_search` | RAG-optimized search with file/container/symbol grouping |
| `search_similar` | Find symbols similar to a reference symbol using fuzzy matching |
| `pattern_search` | Substring pattern match on symbol names |
| `sqry_query` | Run a structured planner query (`kind:function has:caller:main` syntax) directly against the graph |

### Code Understanding

| Tool | Description |
|------|-------------|
| `explain_code` | Explain a symbol with context and relationships |
| `relation_query` | Query semantic relations (callers, callees, imports, exports, returns) |
| `show_dependencies` | Show dependency tree for a file or symbol |
| `dependency_impact` | Analyze what would break if a symbol is changed/removed |

### Navigation

| Tool | Description |
|------|-------------|
| `get_definition` | Get symbol definition location |
| `get_references` | Find all references to a symbol |
| `get_hover_info` | Get hover information for a symbol |
| `get_document_symbols` | Get all symbols in a document |
| `get_workspace_symbols` | Search symbols across workspace |

### Call Hierarchy

| Tool | Description |
|------|-------------|
| `call_hierarchy` | Get full call hierarchy for a symbol |
| `direct_callers` | Find direct callers of a symbol |
| `direct_callees` | Find direct callees of a symbol |

### Graph Operations

| Tool | Description |
|------|-------------|
| `export_graph` | Export dependency subgraph (JSON, DOT, D2, Mermaid) |
| `subgraph` | Extract focused subgraph around symbols for RAG retrieval |
| `trace_path` | Find ranked call paths between two symbols |
| `cross_language_edges` | List cross-language call edges (JS/Python/C++ interop) |

### Introspection

| Tool | Description |
|------|-------------|
| `list_files` | List files in workspace with pagination |
| `list_symbols` | List symbols with pagination |
| `get_graph_stats` | Get graph statistics |
| `get_insights` | Get codebase health indicators |
| `complexity_metrics` | Get cyclomatic complexity metrics |
| `workspace_status` | Report logical workspace identity, source roots, and per-root index status |

### Index & Diff

| Tool | Description |
|------|-------------|
| `get_index_status` | Get current status and metadata of the symbol index |
| `rebuild_index` | Rebuild the semantic index |
| `semantic_diff` | Compare semantic changes between git commits/branches |
| `expand_cache_status` | Get macro expansion cache statistics and status |

### Code Analysis

| Tool | Description |
|------|-------------|
| `find_duplicates` | Find duplicate code (function bodies, signatures, structs) |
| `find_cycles` | Find circular dependencies (call cycles, import cycles, module cycles) |
| `find_unused` | Find unused/dead code via reachability analysis |
| `is_node_in_cycle` | Check if a specific symbol is part of a cycle |

### Natural Language

| Tool | Description |
|------|-------------|
| `sqry_ask` | Translate natural language queries into sqry commands |

### MCP Prompts (Claude Code `/` menu)

These prompts appear as `/mcp__sqry__<prompt_name>` in Claude Code when prompts
are enabled in the MCP server.

Codex and Gemini typically invoke these capabilities via direct MCP tool calls
rather than slash-prompt aliases.

| Prompt | Purpose |
|--------|---------|
| `semantic_search` | Guided structural search prompt (AST-based) |
| `find_callers` | Guided caller discovery for a symbol |
| `find_callees` | Guided callee discovery for a symbol |
| `trace_path` | Guided shortest-path tracing between symbols |
| `explain_symbol` | Guided symbol explanation workflow |
| `code_impact` | Guided dependency impact workflow |
| `ask` | Guided natural-language translation prompt |

### MCP Feature Flags

These environment variables can disable specific MCP tool groups at runtime:

| Variable | Controls |
|----------|----------|
| `SQRY_MCP_ENABLE_GRAPH` | `trace_path`, `subgraph` |
| `SQRY_MCP_ENABLE_EXPORT` | `export_graph` |
| `SQRY_MCP_ENABLE_CROSS_LANGUAGE` | `cross_language_edges` |
| `SQRY_MCP_ENABLE_SEMANTIC_DIFF` | `semantic_diff` |
| `SQRY_MCP_ENABLE_DEPENDENCY_IMPACT` | `dependency_impact` |
| `SQRY_MCP_ENABLE_SQRY_ASK` | `sqry_ask` |

---

## LSP Capabilities (Language Server Protocol)

sqry-lsp provides IDE integration with standard LSP capabilities and custom sqry extensions.

### Standard LSP Capabilities

#### Navigation

| Capability | Description |
|------------|-------------|
| Go to Definition | Jump to symbol definition |
| Find References | Find all references to a symbol |
| Workspace Symbol Search | Search symbols across the entire workspace |
| Document Symbols | Outline view of symbols in current file |

#### Information

| Capability | Description |
|------------|-------------|
| Hover | Show symbol documentation and type information |

#### Code Actions

| Capability | Description |
|------------|-------------|
| Show References | Quick action to show all references |
| Show Callers | Quick action to show all callers |
| Show Call Hierarchy | Incoming/outgoing call hierarchy |

### Custom sqry LSP Endpoints

These endpoints are exposed as direct JSON-RPC custom methods (i.e., the method name appears as the JSON-RPC `method` field). They are NOT invoked through `workspace/executeCommand`. The 4 `sqry.*` command-style actions handled by `workspace/executeCommand` are listed separately under §Code Actions.

#### Search & Query

| Endpoint | Description |
|----------|-------------|
| `sqry/search` | Semantic search with filtering |
| `sqry/hierarchicalSearch` | RAG-optimized search with grouping |
| `sqry/patternSearch` | Wildcard pattern search |
| `sqry/ask` | Natural language to sqry command translation |

#### Symbol Analysis

| Endpoint | Description |
|----------|-------------|
| `sqry/explainSymbol` | Explain symbol with context |
| `sqry/similarSymbols` | Find similar symbols via fuzzy matching |
| `sqry/references` | Query relationships (callers, callees, imports, exports, returns). Params: `{ relation: RelationKind, target: string, path?: string, limit?: usize }`. Returns `{ relation, results: SqrySearchItem[], total, truncated, used_index }`. |
| `sqry/directCallers` | Find direct callers |
| `sqry/directCallees` | Find direct callees |
| `sqry/batchCallerCalleeCount` | Batch-count direct callers and callees for a list of symbols. Params: `{ symbols: SymbolRef[], path?: string }`. Returns `{ counts: { name, callers, callees }[] }`. |

#### Graph Operations

| Endpoint | Description |
|----------|-------------|
| `sqry/graphStats` | Get graph statistics |
| `sqry/graphExport` | Export graph in multiple formats |
| `sqry/subgraph` | Extract focused subgraph |
| `sqry/tracePath` | Find K-shortest paths between symbols |
| `sqry/showDependencies` | Show dependency tree |
| `sqry/dependencyImpact` | Analyze change impact |

#### Code Quality

| Endpoint | Description |
|----------|-------------|
| `sqry/listDuplicateGroups` | Find duplicate code |
| `sqry/listCircularDependencies` | Find circular dependencies |
| `sqry/listUnusedSymbols` | Find unused code |
| `sqry/isNodeInCycle` | Check if symbol is in a cycle |
| `sqry/complexityMetrics` | Calculate cyclomatic complexity |
| `sqry/getInsights` | Get codebase health indicators |

#### Workspace Management

| Endpoint | Description |
|----------|-------------|
| `sqry/listFiles` | List files with pagination |
| `sqry/listSymbols` | List symbols with pagination |
| `sqry/listFilesByLanguage` | List files filtered by language |
| `sqry/listCrossLanguageRelations` | List cross-language edges |
| `sqry/indexStatus` | Get index metadata and status |
| `sqry/workspaceStatus` | Report logical workspace identity (`workspace_id_short`, `workspace_id_full`), source roots, and per-root aggregate index status. Params: `{ workspace_id?: string }` (optional client-side sanity-check value). Returns `SqryWorkspaceStatusResult` (flattens `WorkspaceStatusInfo`). |
| `sqry.index` | Rebuild semantic index with progress |

#### Diff & Comparison

| Endpoint | Description |
|----------|-------------|
| `sqry/semanticDiff` | Compare symbols between git refs |

### LSP Configuration Keys

These settings can be supplied via `workspace/didChangeConfiguration` or the
LSP server config file.

| Key | Description | Example |
|-----|-------------|---------|
| `sqry.path` | Path to the `sqry` binary | `"sqry"` |
| `sqry.indexRoot` | Override index root | `"/repo"` |
| `sqry.search.limit` | Search result limit | `200` |
| `sqry.search.timeout` | Search timeout (ms) | `5000` |
| `sqry.log.level` | Log level | `"info"` |
| `sqry.document.maxBytes` | Legacy max bytes (source files) | `524288` |
| `sqry.document.sourceMaxBytes` | Source file size limit | `524288` |
| `sqry.document.dataMaxBytes` | Data file size limit | `10485760` |
| `sqry.callHierarchy.maxResults` | Call hierarchy result limit | `200` |
| `sqry.callHierarchy.timeoutMs` | Call hierarchy timeout (ms) | `5000` |
| `sqry.callHierarchy.includeDetail` | Include call detail | `true` |
| `sqry.projectRootMode` | Project-root detection mode for the LSP server. Accepted values: `"gitRoot"` (default), `"workspaceFolder"`, `"workspaceRoot"`. The VS Code extension's `sqry.workspaceClassification.projectRootMode` setting uses the short forms `"gitRoot" \| "folder" \| "explicit"` for the same enum; the per-workspace `.code-workspace` `sqry.workspace.projectRootMode` block always wins when present. | `"gitRoot"` |
| `sqry.indexRoot` | LSP server config key (parsed by `sqry-lsp/src/config.rs::apply_index_root` from `workspace/didChangeConfiguration` and the LSP server config file). Sets the index root directly. The VS Code extension also exposes `sqry.indexRoot` as a contributed setting and forwards its value through to `sqry lsp` via the LSP configuration channel. Empty string means no override. | `""` |
| `sqry.workspaceFolderExcludes` | VS Code-side setting only. Glob patterns matched against `WorkspaceFolder.uri.fsPath`. Folders matching any pattern are excluded from every sqry enumeration loop in the extension (auto-index, status fan-out, manual rebuild). Not consumed by the LSP server directly. | `[]` |
| `sqry.workspaceClassification` | VS Code-side setting only. Inline classification block — `sourceRoots`, `memberFolders`, `exclusions`, `projectRootMode` — parallel to the `.code-workspace` `sqry.workspace` block. Surfaces the user-editable form (the `Sqry: Edit Workspace Classification` command writes from this form into the `.code-workspace` block when one exists). When no `.code-workspace` is open today, the extension does NOT synthesize a `LogicalWorkspace` from this setting and forward it to `sqry lsp` — `sqry lsp` falls through to its `.sqry-workspace` / anonymous-multi-root branches. The contributed setting ships now so it is present when that wiring lands as a follow-up task. | `null` |

> Configurable per-workspace cross-repo: sqry treats each opened tree as a single repo by default. Cross-repo classification is enabled by either committing a `.sqry-workspace` registry file (CLI / standalone LSP) or by adding a `sqry.workspace` block inside a `.code-workspace` (VS Code). Both surfaces deserialize into the same `LogicalWorkspace` value before reaching the runtime. <!-- claim:configurable-cross-repo test:resolve_logical_workspace_short_circuits_in_documented_order -->

See [docs/cli/workspace.md](cli/workspace.md) for end-to-end configuration flows and [docs/development/code-map/workspace-aware.md](development/code-map/workspace-aware.md) for the data-flow code map.

---

## VS Code Extension

The VS Code extension (`sqry-vscode`) builds on `sqry lsp` and exposes:

### Commands

| Command | Description |
|---------|-------------|
| `Sqry: Query…` | Run structured sqry queries |
| `Sqry: Search Workspace…` | Fuzzy symbol search |
| `Sqry: Find Semantic References` | Relation lookup from cursor |
| `Sqry: Index Workspace` | Build/rebuild index with progress |
| `Sqry: Refresh Index Stats` | Refresh index status panel |
| `Sqry: Clear Results` | Clear the results panel |

### Panels & UX

- **Semantic Results panel** with symbol + text matches
- **Index status** summary (counts, health, build age)
- **Cross-language relations** listing
- **Duplicate/cycle/unused** diagnostics
- **CodeLens** caller counts and quick actions
- **Auto-index on open** (prompt/always/never)

---

## Core API (`sqry-core`)

### Code Graph

- `CodeGraph` - Arena-based graph with CSR storage
- `GraphBuilder` - Trait for language-specific graph building
- `NodeKind` - 28 symbol kinds (Function, Class, Method, etc.)
- `EdgeKind` - 26 edge types with metadata (Calls, Imports, Inherits, FfiCall, HttpRequest, etc.)
- `GraphSnapshot` - Immutable snapshot for concurrent reads

### Search & Query

- `SemanticSearch` - Natural language search
- `QueryExecutor` - Execute structured queries
- `RelationQuery` - Query symbol relationships
- `SimilaritySearch` - Find similar symbols

### Session & Cache

- `SessionManager` - Workspace session management
- `CacheManager` - AST and graph metadata caching
- `PrewarmOrchestrator` - Cache warming for performance

### Plugin System

- `PluginManager` - Language plugin registry
- `LanguagePlugin` - Trait for language support
- `GraphBuildHelper` - Common graph building utilities

---

## Supported Languages (35 plugins)

| Category | Languages |
|----------|-----------|
| **Systems** | C, C++, Rust, Zig |
| **JVM** | Java, Kotlin, Scala, Groovy |
| **Web Frontend** | JavaScript, TypeScript, HTML, CSS, Vue, Svelte |
| **Backend** | Python, Ruby, PHP, Go, Elixir, Perl |
| **Mobile** | Swift, Dart |
| **Functional** | Haskell, Elixir, Scala |
| **Scripting** | Shell (Bash), Lua, R |
| **Database** | SQL, Oracle PL/SQL |
| **Infrastructure** | Terraform, Puppet |
| **Enterprise** | SAP ABAP, Salesforce Apex, ServiceNow (Xanadu) |
| **.NET** | C# |

---

## Query Syntax

sqry supports a structured query syntax with multiple predicates, operators, and combinators.

### Basic Predicates

```
# Find by name (exact match)
name:authenticate

# Find by kind
kind:function

# Find by file path (glob patterns supported)
path:src/auth/*.rs
file:src/auth/*.rs   # alias for path

# Find by language
lang:rust
language:python      # alias for lang

# Find by visibility
visibility:public
```

### Relationship Predicates

```
# Find symbols that call a function
callers:database_query

# Find symbols called by a function
callees:main

# Find files that import a module
imports:react

# Find files that export a symbol
exports:UserService

# Find functions with specific return type
returns:Result
```

### Advanced Predicates

```
# Find types implementing a trait/interface
impl:Iterator

# Find symbols within a scope
scope.type:class
scope.name:UserService
scope.parent:AuthModule
scope.ancestor:AppModule

# Find symbols with references
references:config
```

### Code Quality Predicates (CD Static Analysis)

```
# Find duplicate code
duplicates:body         # duplicate function bodies
duplicates:signature    # duplicate signatures
duplicates:struct       # duplicate struct definitions

# Find unused code
unused:public           # unused public symbols
unused:private          # unused private symbols
unused:function         # unused functions
unused:all              # all unused symbols

# Find circular dependencies
circular:calls          # call cycles
circular:imports        # import cycles
circular:all            # all cycles
```

### Boolean Operators

```
# AND - both conditions must match
kind:function AND visibility:public

# OR - either condition matches
kind:class OR kind:struct

# NOT - negation
kind:function AND NOT name:test_

# Parentheses for grouping
(kind:function OR kind:method) AND visibility:public
```

### Operators

| Operator | Description | Example |
|----------|-------------|---------|
| `:` | Equality/glob match | `name:authenticate` |
| `~=` | Regex match | `name~=/^test_/` |
| `>` | Greater than | `lines>100` |
| `<` | Less than | `complexity<10` |
| `>=` | Greater or equal | `lines>=50` |
| `<=` | Less or equal | `complexity<=5` |

### Regex Patterns

```
# Match names starting with "test_"
name~=/^test_/

# Match names ending with "Handler"
name~=/Handler$/

# Case-insensitive matching
name~=/error/i

# Multiline matching
text~=/TODO.*FIXME/m
```

### Shorthand Syntax

```
# Bare word defaults to name regex
Error           # equivalent to: name~=/Error/

# Quoted string defaults to name equality
"UserService"   # equivalent to: name:UserService
```

---

## Output Formats

### Text Formats

| Format | Use Case |
|--------|----------|
| `text` | Default human-readable output with color support |
| `json` | Machine-readable JSON for API integration |
| `csv` | RFC 4180 compliant CSV for spreadsheets |
| `tsv` | Tab-separated values for Unix pipelines |

### Graph Export Formats

| Format | Use Case |
|--------|----------|
| `dot` | Graphviz DOT format for visualization |
| `d2` | D2 diagram format |
| `mermaid` | Mermaid diagram format for documentation |
| `json` | JSON graph export for programmatic use |

### Output Options

| Option | Description |
|--------|-------------|
| `--preview` | Show code context (default 3 lines) |
| `--headers` | Include header row in CSV/TSV |
| `--columns` | Select which columns to output |
| `--sort` | Sort results by field |
| `--pager` | Enable/disable paging |

### Color Themes

| Theme | Description |
|-------|-------------|
| `default` | Auto-detect based on terminal |
| `dark` | Optimized for dark backgrounds |
| `light` | Optimized for light backgrounds |
| `none` | Disable colors |
