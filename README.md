# sqry

[![MCP Registry](https://img.shields.io/badge/MCP-Registry-blue)](https://registry.modelcontextprotocol.io/v0.1/servers/io.github.verivus-oss%2Fsqry/versions/latest)
[![crates.io](https://img.shields.io/crates/v/sqry-cli.svg)](https://crates.io/crates/sqry-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

sqry is a local semantic code search tool. It parses source code into AST-backed symbol and relationship graphs so you can ask code questions that text search cannot answer reliably.

Everything runs on your machine. Indexing and querying make no network calls, and sqry sends no telemetry.

Website: https://sqry.dev

## Current Capabilities

- 37 language plugins, 28 with full relation support and 9 with symbol extraction plus imports. By build cost, 29 are on the default fast path, `json` is compiled but excluded by default as high-wall-clock, and 7 specialty plugins sit behind Cargo features. The `sqry://meta/manifest` MCP resource reports these counts for the binary you have installed.
- Structural queries over symbol kind, language, visibility, names, return types, references, and relations.
- Graph analysis for callers, callees, imports, exports, call paths, cycles, unused symbols, duplicates, impact, semantic diff, and focused subgraphs.
- Edge-backed `returns:<TypeName>` and resolution-aware `resolved_via:<kind>` predicates for supported graph paths. See [Call resolution predicates](#call-resolution-predicates).
- Cross-language FFI edges. The C, C++, C#, Elixir, Haskell, Kotlin, Lua, PHP, R, Ruby, and Rust plugins emit `FfiCall` edges from mechanisms such as `extern "C"`, `DllImport` / P/Invoke, JNI, Ruby FFI `attach_function`, and R `.Call` / `Rcpp`. Query them with `sqry graph cross-language` or the `cross_language_edges` MCP tool.
- Structural shape matching with `sqry shape-match`, which finds functions with similar body structure independently of their identifiers.
- A declarative rule layer. `sqry rules run` executes shipped rule IDs and packs, or your own TOML rule packs, against the graph; the `rules_run` MCP tool exposes the same engine.
- Go context propagation analysis (`sqry context-propagation`) for call sites that drop `context.Context`.
- Rust macro-boundary analysis: `sqry cache expand` builds a macro expansion cache, `--expand-cache` materializes macro-generated symbols as searchable nodes, and `--cfg` marks `#[cfg]`-gated symbols active or inactive. Both index-path options are execution-free.
- Visualization and export as Mermaid, Graphviz DOT, or D2 (`sqry visualize`, `sqry export`).
- An interactive shell (`sqry shell`) and batch runner (`sqry batch`) that keep the session cache warm across many queries.
- Incremental indexing with `sqry update` and `sqry watch`.
- Workspace-aware indexing through `.sqry-workspace` registries and VS Code `.code-workspace` `sqry.workspace` blocks. <!-- claim:multi-root-supported test:resolve_logical_workspace_short_circuits_in_documented_order -->
- Daemon-backed shared graph loading through `sqryd` for editor, MCP, and repeated-agent workflows.
- One-shot repository orientation with `sqry overview` (and the matching `generate_overview` MCP tool for agents): a single report of the load-bearing hubs, path/package subsystems, complexity hotspots, potential issues, and ready-to-run follow-up queries.
- MCP integration for AI assistants. Standalone `sqry-mcp` currently exposes 39 tools; daemon-hosted MCP exposes a 17-tool subset. Use `tools/list`, `sqry-mcp --list-tools`, or `sqry://meta/manifest` as the authoritative catalog.
- LSP and VS Code extension support for editor workflows.

## When To Use sqry

Use sqry when you need structural answers:

```bash
sqry index .
sqry overview            # orient in an unfamiliar repo, then run the queries it suggests
sqry query "kind:function AND visibility:public AND lang:rust"
sqry query "returns:Result"
sqry graph direct-callers authenticate
sqry graph trace-path main handle_request
sqry visualize "callers:authenticate" --format mermaid
```

Use ripgrep for simple text search, ast-grep for syntax rewrite patterns, language linters for policy enforcement, and an IDE language server for full editor semantics. sqry stays focused on local semantic code search.

## Install

### Linux And macOS

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
```

The shell installer downloads release assets, verifies SHA256 checksums by default, and installs:

- `sqry`
- `sqry-mcp`
- `sqry-lsp`
- `sqryd`

Supported release platforms are Linux `x86_64`/`arm64` (glibc and musl builds), macOS `x86_64`/`arm64`, and Windows `x86_64`.

Useful options:

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | \
  bash -s -- --component all --version vX.Y.Z --verify-signatures
```

`--verify-signatures` requires `gh` or Cosign. It verifies the current `release-artifacts.attestation.json` GitHub artifact attestation from `release-distribute.yml`; older releases can still fall back to legacy per-asset Cosign bundles. SHA256 verification remains the default integrity check.

### Windows

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
.\install.ps1 -Component all -VerifySignatures
```

The PowerShell installer downloads the Windows `x86_64` release ZIP, verifies checksums by default, and installs `sqry.exe`, `sqry-mcp.exe`, `sqry-lsp.exe`, and `sqryd.exe` into the user install directory.

Use a pinned release when reproducibility matters:

```powershell
.\install.ps1 -Component all -Version vX.Y.Z -VerifySignatures
```

### Homebrew

Homebrew is the package-manager surface currently backed by the public release manifest.

```bash
brew tap verivus-oss/sqry
brew install sqry
```

The formula installs `sqry`, `sqry-mcp`, `sqry-lsp`, and `sqryd` for Linux `x86_64`/`arm64` and macOS `x86_64`/`arm64`.

Scoop, Winget, AUR, Nix, and Snap are not advertised as public release assets until their release-control surfaces are complete and validated.

### Build From Source

```bash
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo install --path sqry-cli
cargo install --path sqry-mcp
cargo install --path sqry-lsp
cargo install --path sqry-daemon
```

Requirements:

- Rust `1.94+` with Edition 2024.
- The repository pins toolchain `1.94.1` in `rust-toolchain.toml`.
- A full workspace build compiles the bundled tree-sitter grammars and language plugins and can require substantial disk space.

## First Index

```bash
# Build the default fast-path index.
sqry index .

# Check status without rebuilding.
sqry index --status --json .

# Force a rebuild after upgrading across graph-format or semantics changes.
sqry index --force .
```

Index artifacts live under `.sqry/`. Add `.sqry/` and legacy `.sqry-index` artifacts to your project ignore rules unless you intentionally share them.

### Index Validation

```bash
sqry search main . --validate fail --auto-rebuild
sqry index --status --metrics-format prometheus .
```

`--validate` takes `off`, `warn` (default), or `fail`. Under `fail` a stale index aborts with exit code 2 instead of returning results computed from it, which is what makes it usable as a CI gate. `--auto-rebuild` rebuilds once and retries, and is a no-op under `--validate off`. Thresholds are tunable: `--threshold-orphaned-files` defaults to 0.20, so validation trips when more than 20% of indexed files no longer exist on disk, and `--threshold-dangling-refs` defaults to 0.05.

Note the limit: these are global flags, but validation is evaluated on the snapshot-loading path taken by search. Passing them to `sqry index` when an index already exists does not trigger a validation pass.

`--metrics-format prometheus` emits OpenMetrics-compatible text for scraping index status.

### Plugin Selection

sqry records the plugin set that built an index so later graph-loading commands reuse the same semantics.

```bash
sqry index .                         # default fast path
sqry index --include-high-cost .     # all compiled non-default plugins
sqry index --exclude-high-cost .     # force the fast path
sqry index --enable-plugin json .    # opt into one plugin
sqry index --disable-plugin json .   # opt out of one plugin
```

The registry has fast plugins, high-wall-clock plugins, and optional specialty plugins. `json` is high-wall-clock. Optional specialty plugins include `apex`, `abap`, `servicenow-xanadu-js`, `servicenow-xml`, `terraform`, `puppet`, and `pulumi` when compiled with the matching Cargo features or `specialty-plugins`.

See [Indexing](docs/user-guide/indexing.md) for artifact cleanup, plugin tiers, and high-cost behavior.

## Query Examples

```bash
# Symbol and attribute predicates.
sqry query "kind:function AND name:parse_*"
sqry query "kind:struct AND lang:rust"
sqry query "kind:function AND returns:Result"

# Relation predicates.
sqry query "callers:authenticate"
sqry query "callees:handle_request"
sqry query "imports:serde"

# Planner-backed query.
sqry plan-query "kind:function callers:main"

# Resolution-aware call filtering where populated by graph metadata.
sqry plan-query "kind:function callers:my_read resolved_via:binding_plane"
```

`returns:<TypeName>` uses graph `TypeOf{Return}` edges.

### Call Resolution Predicates

`resolved_via:<kind>` filters call edges by **what kind of binding produced the edge**. That is a different question from how confident the resolver is: a `duck_typed` edge is not a low-confidence `direct` edge, it is a record that the call was bound by Python duck typing rather than by a syntactic reference. Knowing the mechanism lets you separate calls the compiler could have checked from calls that only resolve at runtime.

The parser accepts eight values:

| Value | Binding mechanism |
|---|---|
| `direct` | Syntactic call resolved by the language plugin |
| `type_match` | Indirect call matched against compatible signatures by type |
| `binding_plane` | Designated-initializer witnesses in the binding plane |
| `virtual_dispatch` | JVM virtual or abstract method dispatch |
| `interface_dispatch` | Go interface dispatch |
| `duck_typed` | Python duck-typed dispatch |
| `structural` | TypeScript structural dispatch |
| `promiscuous_elided` | Fan-out cap exceeded, so the resolver recorded a diagnostic self-edge instead of N targets |

```bash
# Callers of my_read that only bind through the binding plane,
# not through a plain syntactic call.
sqry plan-query "kind:function callers:my_read resolved_via:binding_plane"
```

`framework` filtering is exposed as a typed MCP parameter. The planner also parses `framework:<id>`, but framework-route metadata is only populated where extractors have run. Treat a non-match as absent metadata, not proof that a framework is unused.

See [Advanced Analysis](docs/user-guide/advanced-analysis.md) for graph predicates, snapshot wording, impact analysis, semantic diff, and visualization.

## Workspaces

Use a workspace when one logical project spans several repositories or folders.

```bash
sqry workspace init .
sqry workspace scan .
sqry workspace status . --json
sqry workspace clean .
```

Workspace configuration can come from a `.sqry-workspace` registry or a VS Code `.code-workspace` file with a `sqry.workspace` block. See [Workspaces](docs/user-guide/workspace.md).

## Daemon

`sqryd` keeps graph state warm for repeated CLI, LSP, MCP, and editor workflows.

```bash
sqry daemon start
sqry daemon load .
sqry daemon status --json
sqry daemon rebuild . --force
sqry daemon logs --follow
```

See [Daemon Mode](docs/user-guide/daemon.md).

## MCP And Editors

Configure MCP clients with:

```bash
sqry mcp setup --tool claude
sqry mcp setup --tool codex
sqry mcp setup --tool gemini
```

Standalone `sqry-mcp` is the full local tool surface. `sqry-mcp --daemon` attaches to `sqryd` and exposes the daemon-hosted subset. `workspace_status.source_root_id` is an opaque display/correlation token, not a path. See [MCP Guide](docs/user-guide/mcp.md) and the component docs in [sqry-mcp/README.md](sqry-mcp/README.md).

MCP search tools keep string predicates and structured JSON parameters separate:

```json
{
  "name": "semantic_search",
  "arguments": {
    "query": "kind:function items:true",
    "filters": {
      "language": ["rust"],
      "visibility": "public"
    }
  }
}
```

Use `items:true` or `is_definition:true` in `query` for definition-only query predicates. When calling `list_symbols`, use the structured `items_only` parameter:

```json
{
  "name": "list_symbols",
  "arguments": {
    "kind": "function",
    "language": "rust",
    "items_only": true
  }
}
```

If a graph was built before the V16 definition-signal snapshot format, definition-only query predicates and `list_symbols.items_only` return a `reindexRequired` / reindex-required advisory rather than trusting stale marker data.

For the VS Code extension, see [sqry-vscode/README.md](sqry-vscode/README.md) and [sqry-vscode/USER_GUIDE.md](sqry-vscode/USER_GUIDE.md).

## User Guides

- [Quick Start](QUICKSTART.md)
- [User Guide Index](docs/user-guide/README.md)
- [Indexing](docs/user-guide/indexing.md)
- [Workspaces](docs/user-guide/workspace.md)
- [Daemon Mode](docs/user-guide/daemon.md)
- [MCP Guide](docs/user-guide/mcp.md)
- [Query Languages](docs/user-guide/query-languages.md)
- [Repository Overview](docs/user-guide/overview.md)
- [Rules](docs/user-guide/rules.md)
- [Context Propagation](docs/user-guide/context-propagation.md)
- [Doctor](docs/user-guide/doctor.md)
- [Revision-Aware Workspaces](docs/user-guide/revision-aware-workspaces.md)
- [Advanced Analysis](docs/user-guide/advanced-analysis.md)
- [Structural Shape Matching](docs/user-guide/shape-match.md)
- [Visualization](docs/user-guide/visualization.md)

## Security And Supply Chain

- Release artifacts carry a GitHub OIDC keyless attestation, published as the `release-artifacts.attestation.json` asset and verifiable with `gh attestation verify` or Cosign. `SHA256SUMS.txt` ships alongside, and both installers verify checksums by default.
- Each release also publishes a release manifest and a release ledger asset recording the stages the build passed through.
- Dependencies are gated in CI by `cargo-vet` (importing the Mozilla, Google, Bytecode Alliance, and ISRG audit sets), `cargo-deny`, and `cargo-audit`. Clippy runs as `-D warnings`.
- The query planner's text parser is fuzzed nightly with `cargo-fuzz`. `cargo-mutants` runs on changed lines per PR and against a canary-crate baseline on master; it currently reports rather than blocks.
- SBOMs (CycloneDX and SPDX) and OpenVEX documents are generated by a separate workflow.

Do not assume an artifact is present unless it appears in the asset list for the release you are installing. To report a vulnerability, see [SECURITY.md](SECURITY.md).

## Project Scope

sqry is an MIT-licensed open-source project focused on one product goal: local semantic code search. It is not a hosted search platform, linter, IDE replacement, or general metrics platform.
