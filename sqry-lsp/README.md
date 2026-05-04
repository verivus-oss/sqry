# sqry LSP Server - by Verivus

**AST-aware Language Server Protocol endpoint for sqry-indexed workspaces**

`sqry-lsp` (binary `sqry-lsp`, also reachable as the `sqry lsp` subcommand of
the main `sqry` CLI) is the Language Server Protocol implementation in the
sqry workspace. It bridges any LSP-compliant editor (VS Code, Neovim,
Helix, Emacs lsp-mode, JetBrains LSP, Sublime LSP, etc.) to the same
unified-graph index that powers `sqry-mcp` and the `sqry` CLI. The
server speaks **standard LSP** (definition, references, document and
workspace symbols, hover, code actions, code lenses, diagnostics) over
the same connection as a curated set of **`sqry/*` JSON-RPC custom
methods** that expose graph-native semantics not covered by the LSP
specification: hierarchical search, `sqry/ask` natural-language
queries, dependency impact, cycle detection, complexity metrics,
`sqry/semanticDiff`, and more.

The server is implemented in Rust on top of `tower-lsp` and shares the
same `sqry-core` graph engine, `sqry-db` derived-fact cache, and
plugin set used by every other sqry surface — `sqry-lsp` is a
transport, not a separate analyzer.

---

## What You Can Do

With sqry-lsp connected to your editor, you can:

- Jump to definitions and references across **all 37 sqry-supported
  languages** (Rust, TypeScript, Python, Java, Go, Kotlin, Swift,
  C/C++, Ruby, PHP, JVM-classpath, etc.) using a single graph index.
- Browse document and workspace symbols backed by the unified graph
  arena rather than per-file textual heuristics.
- Trigger custom code actions (`Find Callers of X`, `Show References
  for X`, `Explain X`) on any symbol; the underlying searches resolve
  via `sqry-db` and respect cross-file unification.
- Send `sqry/*` custom JSON-RPC requests directly from editor
  extensions (e.g. `sqry-vscode`) to drive panels for hierarchical
  search, dependency impact, semantic diff, and graph export.
- Run the server as a thin shim against a long-lived `sqryd` daemon
  with `--daemon`, sharing one preloaded workspace graph across every
  editor session for zero-rebuild startup.

---

## Quick Start

### 1. Install

```bash
cargo install --path sqry-cli
cargo install --path sqry-lsp
sqry --version
sqry-lsp --help
```

### 2. Index Your Project

```bash
cd /path/to/your/project
sqry index
```

The LSP server reads `.sqry/graph/snapshot.sqry` (V10) and the
companion `.sqry/graph/derived.sqry` derived-cache file. If the
snapshot is missing or stale, the server still starts but every
graph-backed handler degrades to "no result" until you re-index.

### 3. Wire Into Your Editor

Editor configuration is editor-specific. The server's startup
arguments are the same shape every LSP client expects: a command and
optional arguments. Two canonical shapes:

**Stdio (default, recommended for most editors)**:

```bash
sqry-lsp --stdio
```

**TCP (for editors that prefer a socket transport)**:

```bash
sqry-lsp --socket 127.0.0.1:9257
```

For a daemon-backed deployment that keeps one warm graph across every
editor session:

```bash
sqry daemon start
sqry daemon load .
sqry-lsp --daemon
```

When `sqry-lsp --daemon` cannot reach a daemon it auto-starts one
unless `SQRY_DAEMON_NO_AUTO_START=1` is set; see
[`docs/cli/daemon.md`](../docs/cli/daemon.md) for the full daemon
lifecycle, socket resolution rules, and shim contract.

> **No manpage.** `sqry-lsp` does not ship a man page; `sqry-lsp --help`
> and this README are the canonical references.
>
> **No `completions` subcommand.** Unlike `sqry`, `sqry-lsp` is a
> single-purpose binary with a flat flag set; there is no shell-completion
> generator subcommand.

---

## CLI Flags

There are two ways to launch the LSP server, and the available flag set
differs between them:

1. **Standalone `sqry-lsp` binary** — the flags defined in
   `sqry-lsp/src/cli.rs::LspOptions` (the clap-derived flag set). This is
   what the table below documents.
2. **`sqry lsp` subcommand** of the parent `sqry` CLI — accepts every
   standalone flag below **plus** any *global* `sqry` flags. The most
   notable global is `--workspace <PATH>`, which is parsed at the parent
   `sqry` CLI root (`sqry-cli/src/args/mod.rs`) and forwarded into
   `LspOptions::workspace` by the `sqry lsp` dispatcher in
   `sqry-cli/src/main.rs`. The standalone `sqry-lsp` binary does **not**
   surface `--workspace` as its own clap arg; the `LspOptions::workspace`
   field is `#[arg(skip)]` and is populated only via the parent CLI
   dispatcher (or, for daemon-hosted sessions, by the daemon router
   constructing `LspOptions` directly).

| Flag | Argument | Purpose |
|------|----------|---------|
| `--stdio` | _(none)_ | Run over stdin/stdout. Default transport when neither `--socket` nor `--daemon` is set. Mutually exclusive with `--socket` and `--daemon`. |
| `--socket` | `ADDR` | Listen on a TCP socket instead of stdio. Use `127.0.0.1:PORT` for localhost-only access. Mutually exclusive with `--daemon`. |
| `--daemon` | _(none)_ | Connect to a running `sqryd` daemon and pump LSP bytes over its shim byte-pump transport. Acts as a CLIENT shim: opens a UDS / named-pipe connection, sends a `ShimRegister { protocol: Lsp, pid }` frame, awaits `ShimRegisterAck`, then forwards stdio bidirectionally. Mutually exclusive with `--stdio` and `--socket`. |
| `--daemon-socket` | `PATH` | Override the daemon UDS / named-pipe path. Resolution precedence: this flag → `$SQRYD_SOCKET` → platform default (`$XDG_RUNTIME_DIR/sqry/sqryd.sock` on Unix XDG, `$TMPDIR/sqry-<uid>/sqryd.sock` on generic Unix, `\\.\pipe\sqry` on Windows). Requires `--daemon`. |
| `--allow-public-bind` | _(none)_ | Suppress the security warning emitted when `--socket` binds to a non-localhost address. **The LSP protocol transmits source code without encryption or authentication.** Set only on trusted private networks. Also reads `SQRY_LSP_ALLOW_PUBLIC_BIND` env var. |
| `--index-root` | `PATH` | **Deprecated.** Explicit index root override; redundant when the LSP `initialize` payload carries `initializationOptions.sqry.workspace` (the modern `sqry-vscode` extension flow) or `initializationOptions.sqry.indexRoot`. The flag still works but emits a `tracing::warn!` on use; new deployments should use the in-band signal. |
| `--config` | `FILE` | Path to a JSON config file with advanced settings. |
| `--log-level` | `LEVEL` | Log verbosity (`error`, `warn`, `info`, `debug`, `trace`). Default `warn`. |

### Parent-CLI-only flags (when invoked as `sqry lsp …`)

| Flag | Argument | Purpose |
|------|----------|---------|
| `--workspace` | `PATH` | **Parsed by the parent `sqry` CLI root**, not the standalone `sqry-lsp` binary. Pins the LSP workspace root explicitly, bypassing `initializationOptions.sqry.workspace` / `sqry.indexRoot` heuristics. Forwarded into `LspOptions::workspace` by the `sqry lsp` dispatcher. Also reads `SQRY_WORKSPACE_FILE`. Useful for non-standard editor wrappers and CI; standalone `sqry-lsp` deployments should use the in-band `initializationOptions.sqry.workspace` signal instead. |

`sqry-lsp --help` is the authoritative reference for the standalone
binary; `sqry lsp --help` shows the parent-CLI shape (standalone flags
plus inherited globals). The standalone table mirrors the clap-derived
flag definitions at `sqry-lsp/src/cli.rs`.

---

## Standard LSP Capabilities

`sqry-lsp` advertises the following capabilities in the `initialize`
response (see `sqry-lsp/src/server.rs:1152-1199` and the
`execute_command_provider` block immediately following):

| Capability | Shape | Notes |
|------------|-------|-------|
| `textDocumentSync` | `Options { openClose: true, change: Incremental }` | Incremental sync; full-document re-parse only on file open/close. |
| `definitionProvider` | `true` | Graph-backed definition lookup via the unified arena. |
| `referencesProvider` | `true` | Graph-backed reference search. The custom `sqry/references` method exposes the same logic with extra metadata. |
| `documentSymbolProvider` | `true` | Symbols from the file's `Defines` / `Contains` edges. |
| `workspaceSymbolProvider` | `Options { resolveProvider: false }` | Workspace-wide symbol search. |
| `hoverProvider` | `true` | Hover info synthesised from the symbol's signature and documentation. |
| `codeActionProvider` | `Options { codeActionKinds: [REFACTOR, EMPTY] }` | See [`workspace/executeCommand` Actions](#workspaceexecutecommand-actions). The `code_action_kinds` filter set advertises `REFACTOR` (for `Find Callers` / `Show References`) and `EMPTY` (for `Explain Symbol`); editors filter out actions outside that set. |
| `codeLensProvider` | `Options { resolveProvider: false }` | Caller-count code lenses for every callable (`Function`, `Method`, `Macro`, `LambdaTarget`) defined in the requested document. Each lens carries a `Command { command: "sqry.showCallers", arguments: [{ uri, position }] }` so editors can pivot from the rendered count into the existing show-callers code action; `data` carries `{ name, count }` so clients and tests can read the count without parsing the title. Counts are derived from `sqry-db`'s `mcp_callers_query` inversion wrapper (see `sqry-lsp/src/handlers/codelens.rs`). |
| `diagnosticProvider` | `Options { identifier: "sqry", interFileDependencies: false, workspaceDiagnostics: false }` | Pull-model diagnostics for symbols defined in the requested document: **(1)** unused-symbol warnings (`severity: Warning`, `code: "sqry::unused"`) sourced from `sqry-db`'s `UnusedQuery` keyed on `UnusedScope::All`; **(2)** cycle-member info diagnostics (`severity: Information`, `code: "sqry::cycle"`) sourced from `CyclesQuery` keyed on `CircularType::Calls`; **(3)** duplicate-group warnings (`severity: Warning`, `code: "sqry::duplicate"`) sourced from `sqry_core::query::build_duplicate_groups_graph` with `DuplicateType::Body`. All three classes carry `source: "sqry"`. See `sqry-lsp/src/handlers/diagnostics.rs`. |
| `workspace.workspaceFolders` | `Options { supported: true, changeNotifications: true }` | Multi-root and dynamic folder support; see [Workspace Folder Changes](#workspace-folder-changes) below. |
| `executeCommandProvider` | `Options { commands: ["sqry.index", "sqry.showCallers", "sqry.showReferences", "sqry.explainSymbol"] }` | Required by LSP 3.17 §workspace_executeCommand for clients to route command requests to the server. |

`callHierarchyProvider` is also advertised (`Simple(true)`) for editors
that drive call hierarchies through the standard LSP type, but the
graph-native `sqry/directCallers` / `sqry/directCallees` /
`sqry/batchCallerCalleeCount` custom methods are recommended for
better performance and metadata fidelity.

### Workspace Folder Changes

The server handles `workspace/didChangeWorkspaceFolders` notifications
(`sqry-lsp/src/server.rs:1168-1174,1422`). When the editor adds or
removes a workspace folder, the session manager re-resolves the
logical workspace and emits a single aggregate `INFO` event on the
`sqry::workspace` tracing target with `workspace_id_short`,
`source_root_count`, `member_count`, `exclusion_count`, and `branch`
fields. Adding folders does not implicitly re-index — re-run `sqry
index` (or trigger the `sqry.index` execute-command) to refresh the
graph after a topology change.

---

## `workspace/executeCommand` Actions

`sqry-lsp` registers four `sqry.*` execute-command actions
(`sqry-lsp/src/handlers/execute_command.rs:19-26` plus the
`code_action.rs` constants at `:9-11`). Editors invoke these via
`workspace/executeCommand` requests, typically wired up through the
matching `CodeAction` returned by `textDocument/codeAction` (with the
exception of `sqry.index`, which is invoked directly by `sqry-vscode`
and similar extensions for explicit "Rebuild Index" buttons).

| Command | Trigger | Behaviour |
|---------|---------|-----------|
| `sqry.index` | Editor extension (e.g. "sqry: Rebuild Index" command palette entry) | Trigger an index rebuild for the active workspace. |
| `sqry.showCallers` | Code action `Find Callers of <name>` (`CodeActionKind::REFACTOR`, `is_preferred: true`) | Resolve the symbol at the cursor, run a references query with `include_declaration: false`, and return `{ command, context, symbol, results: { count, includeDeclaration, locations, nextPageToken } }`. |
| `sqry.showReferences` | Code action `Show References for <name>` (`CodeActionKind::REFACTOR`) | Same as `showCallers` but with `include_declaration: true`. |
| `sqry.explainSymbol` | Code action `Explain <name>` (`CodeActionKind::EMPTY`) | Resolve the symbol at the cursor and return `{ name, qualifiedName, language, signature, documentation }`. |

All four commands take a single positional argument of shape
`{ uri: string, position: { line: u32, character: u32 } }`. Unknown
command names return `unsupported command: <name>`.

---

## Custom `sqry/*` JSON-RPC Methods (29)

`sqry-lsp` registers 29 custom methods on top of standard LSP, all
sharing the JSON-RPC envelope used by the rest of the LSP connection.
The full registration table lives in
`sqry-lsp/src/lib.rs:571-685`; the table below mirrors that
registration order. Unless otherwise noted, request bodies follow the
same field shape as the matching `sqry-mcp` tool (see
[`sqry-mcp/README.md`](../sqry-mcp/README.md)) and responses are
plain JSON values rather than typed LSP responses.

| Method | Purpose | Request shape (summary) | Response shape (summary) |
|--------|---------|-------------------------|--------------------------|
| `sqry/search` | Fuzzy semantic symbol search with predicates and filters. | `{ query, filters?, max_results? }` | `{ items: [...], truncated: bool, total: u32 }` |
| `sqry/references` | Graph-backed reference lookup at a symbol position; richer metadata than the standard `textDocument/references`. | `{ uri, position, include_declaration? }` | `Vec<Location>` plus `{ symbol, count, nextPageToken }` envelope. |
| `sqry/indexStatus` | Index build state, snapshot path, and freshness metadata. | `{}` | `{ exists, path, language_counts, file_count, ... }` |
| `sqry/workspaceStatus` | Workspace identity, root, and load state (workspace_id, source roots, members, exclusions, branch). | `{}` | `{ workspace_id, source_roots, member_folders, exclusions, branch }` |
| `sqry/listFiles` | Enumerate indexed files. | `{ language?, path_prefix?, max_results? }` | `{ files: [...], truncated, total }` |
| `sqry/listSymbols` | Enumerate indexed symbols across the workspace. | `{ kind?, language?, max_results? }` | `{ symbols: [...], truncated, total }` |
| `sqry/listFilesByLanguage` | Counts and file lists grouped by language. | `{}` | `{ languages: [{ language, file_count, files: [...] }] }` |
| `sqry/listCrossLanguageRelations` | Enumerate cross-language edges (FFI, HTTP, gRPC, DB, etc.). | `{ kind?, max_results? }` | `{ relations: [...], truncated, total }` |
| `sqry/listDuplicateGroups` | Duplicate-code detection groups. | `{ min_size?, max_results? }` | `{ groups: [...], truncated, total }` |
| `sqry/listCircularDependencies` | Find cycles (call / import / module). UTF-16 cycle-member columns are wire-correct (PN2). | `{ kind?, max_results? }` | `{ cycles: [...], truncated, total }` |
| `sqry/listUnusedSymbols` | Dead-code detection. | `{ language?, max_results? }` | `{ unused: [...], truncated, total }` |
| `sqry/hierarchicalSearch` | RAG-optimised grouped search (file → symbols). | `{ query, filters?, max_results? }` | `{ groups: [{ file, hits: [...] }], truncated, total }` |
| `sqry/ask` | Natural-language query → sqry command translation. | `{ question, context? }` | `{ command, args, explanation }` |
| `sqry/directCallers` | Direct callers of a symbol (depth = 1). | `{ name | uri+position, max_results? }` | `{ callers: [...], truncated, total }` |
| `sqry/directCallees` | Direct callees of a symbol (depth = 1). | `{ name | uri+position, max_results? }` | `{ callees: [...], truncated, total }` |
| `sqry/batchCallerCalleeCount` | Batched caller / callee counts for a list of symbols (used by `sqry-vscode` to render gutter badges in a single round-trip). | `{ symbols: [{ name | uri+position }] }` | `{ counts: [{ symbol, callers, callees }] }` |
| `sqry/graphStats` | Graph-wide statistics (node counts by kind, edge counts by kind, file counts, language breakdown). | `{}` | `{ nodes_by_kind, edges_by_kind, file_count, language_breakdown }` |
| `sqry/patternSearch` | Substring / pattern match on symbol names. | `{ pattern, max_results? }` | `{ items: [...], truncated, total }` |
| `sqry/dependencyImpact` | Reverse dependency impact for a symbol or file. | `{ name | path, depth? }` | `{ impacted: [...], truncated, total }` |
| `sqry/explainSymbol` | Symbol details with surrounding context (signature, documentation, neighbours). | `{ name | uri+position }` | `{ name, qualifiedName, language, signature, documentation, neighbours }` |
| `sqry/tracePath` | Call-path tracing between two symbols. | `{ from, to, max_paths? }` | `{ paths: [...], truncated, total }` |
| `sqry/graphExport` | Export DOT / D2 / Mermaid / JSON subgraph. | `{ format, root?, depth? }` | `{ format, content }` |
| `sqry/subgraph` | Focused subgraph extraction around a node. | `{ root, depth?, kinds? }` | `{ nodes: [...], edges: [...], truncated }` |
| `sqry/isNodeInCycle` | Cycle membership predicate for a single symbol. | `{ name | uri+position }` | `{ in_cycle: bool, cycle_members?: [...] }` |
| `sqry/similarSymbols` | Find similar symbols (signature / structure based). | `{ name | uri+position, max_results? }` | `{ items: [...], truncated, total }` |
| `sqry/showDependencies` | Dependency tree for a file or symbol. | `{ name | path, depth? }` | `{ tree: { ... }, truncated, total }` |
| `sqry/complexityMetrics` | Per-symbol or per-file complexity metrics. | `{ name? | path?, max_results? }` | `{ metrics: [...], truncated, total }` |
| `sqry/getInsights` | Codebase health indicators. | `{}` | `{ insights: [...] }` |
| `sqry/semanticDiff` | Semantic diff between two graph snapshots (e.g. two git refs). | `{ base, head, options? }` | `{ added, removed, changed, summary }` |

Field-level shapes for symbol-keyed methods follow the
`sqry-mcp` family: handlers accept either `name` (with optional
`file_path` disambiguation when multiple symbols share a name) or
`{ uri, position }` for cursor-anchored lookups. For exact request /
response Rust types, see the matching handlers under
`sqry-lsp/src/handlers/*.rs` — each `handle_*` function declares its
typed `params` struct and `Output` value.

---

## Environment Variables

| Variable | Purpose | Default | Security |
|----------|---------|---------|----------|
| `SQRY_LSP_PERF_LOG` | Per-handler performance logging. Set to `1`, `true`, or `yes` (case-insensitive) to write a tab-separated entry to `$XDG_DATA_HOME/sqry/lsp-perf.log` (or `~/.local/share/sqry/lsp-perf.log`) for every LSP request. Source: `sqry-lsp/src/handlers/index.rs:10,16`. | unset (disabled) | Logs include URIs and timing. Disable on shared workstations if URIs are sensitive. |
| `SQRY_LSP_ALLOW_PUBLIC_BIND` | Suppresses the warning emitted when `--socket` binds to a non-localhost address. Source: `sqry-lsp/src/security.rs:120,148`. Equivalent to passing `--allow-public-bind`. | `false` | **SECURITY-RELEVANT.** The LSP protocol transmits source code without encryption or authentication. Setting this var to a truthy value allows the server to bind to private-network or fully public addresses without warning. Use only on trusted private networks (home network behind a firewall, isolated CI runner) or behind a TLS-terminating reverse proxy. Never enable on multi-tenant or untrusted networks. |
| `SQRY_DAEMON_NO_AUTO_START` | Disables auto-start when `--daemon` is set and no daemon is reachable. Inherited from `sqry-daemon-client`. | unset (auto-start enabled) | None. |
| `SQRYD_SOCKET` | Client-side override for the daemon UDS / named-pipe path consumed by `--daemon`. Distinct from the daemon-side `SQRY_DAEMON_SOCKET`. | unset (platform default) | None (path string only). |
| `RUST_LOG` | Standard `tracing-subscriber` log filter. Combined with `--log-level` (the latter sets the default level). | `warn` | None. |

---

## Daemon Mode

When `sqry-lsp --daemon` is set, the binary acts as a **client-side
shim**: it does not run any LSP logic itself, but instead opens a
connection to a long-lived `sqryd` daemon (default UDS path
`$XDG_RUNTIME_DIR/sqry/sqryd.sock` on Unix XDG, named pipe
`\\.\pipe\sqry` on Windows; override with `--daemon-socket` or
`$SQRYD_SOCKET`), sends a `ShimRegister { protocol: Lsp, pid }`
frame, awaits `ShimRegisterAck`, then forwards bytes between the
editor's stdio and the daemon's hosted LSP server. The actual
`tower_lsp::Server` lives inside the daemon and is hosted by
`daemon_host::host_on_streams`. This means a single warm graph (and a
single derived-cache) is shared across every editor session for that
workspace — startup cost drops from "rebuild on connect" to "open a
socket".

If no daemon is reachable, the shim attempts one auto-start (`sqryd
start --detach`) before failing, unless `SQRY_DAEMON_NO_AUTO_START=1`
is set. Source-tree changes are picked up by the daemon's
`SourceTreeWatcher` (2-second debounce, in-flight rebuild
cancellation, `WorkspaceState::{Loaded, Rebuilding, Evicted, Failed}`
state machine). On `WorkspaceEvicted` (memory budget breach), the
next request returns IPC error code `-32004` and the editor must
reload the workspace.

For complete daemon configuration (config file location, memory
limits, log rotation, pidfile / lockfile lifecycle, ready-pipe
detach), see [`docs/cli/daemon.md`](../docs/cli/daemon.md).

---

## Implementation Notes

- **Transport**: tower-lsp on top of `tokio` for stdio, TCP, and
  daemon byte-pump shapes. Single registration table at
  `sqry-lsp/src/lib.rs::build_sqry_service` is the authoritative
  source for both standard LSP capabilities and `sqry/*` custom
  methods.
- **Session model**: one `SessionManager` per connection (per-shim
  for daemon mode). Session state is not shared across connections
  — sharing is a deferred performance optimisation, not a
  correctness requirement.
- **Workspace classification gate**: every handler runs through
  `SessionManager::evaluate_handler_gate` before touching the graph;
  requests against member folders or excluded paths short-circuit to
  `Ok(None)` (LSP-standard "no result") without any per-folder
  filesystem probe.
- **sqry-db routing**: `list_unused_symbols` and
  `list_circular_dependencies` route through the `sqry-db`
  `DerivedQuery` cache (PN2 wired the wire contract for truncation
  totals, `InvalidParams` mapping, and UTF-16 cycle-member columns).
  Other handlers route through direct snapshot enumeration when
  NodeId-anchored.

---

## Testing

```bash
cargo test -p sqry-lsp
```

Coverage includes:

- Standard LSP request/response round-trips against a mocked transport.
- All 29 `sqry/*` custom methods.
- All four `workspace/executeCommand` handlers, including
  argument-parsing edge cases (missing `uri`, missing `position`,
  out-of-range `line` / `character` clamping to `u32::MAX`).
- Workspace classification gate (member-folder and exclusion-path
  short-circuits).
- Daemon shim handshake (`ShimRegister` / `ShimRegisterAck` framing).

---

## References

- **LSP Specification**: https://microsoft.github.io/language-server-protocol/
- **sqry MCP Server**: [`sqry-mcp/README.md`](../sqry-mcp/README.md) — same graph backend, different transport.
- **Daemon CLI Reference**: [`docs/cli/daemon.md`](../docs/cli/daemon.md)
- **Workspace Wrapper Migration**: `docs/cli/workspace-wrapper-migration.md` (referenced by the deprecation note on `--index-root`).
- **VS Code Extension**: `sqry-vscode/` — primary client of the `sqry/*` custom methods.

---

## License

MIT - See root LICENSE file

---

**Last Updated**: 2026-05-04
**Custom methods**: 29 (`sqry/*`)
**LSP capabilities advertised**: 11 standard + 1 call-hierarchy
**Execute-command actions**: 4 (`sqry.index`, `sqry.showCallers`, `sqry.showReferences`, `sqry.explainSymbol`)
