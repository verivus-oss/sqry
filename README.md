# sqry

[![MCP Registry](https://img.shields.io/badge/MCP-Registry-blue)](https://registry.modelcontextprotocol.io/v0.1/servers/io.github.verivus-oss%2Fsqry/versions/latest)
[![crates.io](https://img.shields.io/crates/v/sqry-cli.svg)](https://crates.io/crates/sqry-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

sqry is a local semantic code search tool. It parses source code into AST-backed symbol and relationship graphs so you can ask code questions that text search cannot answer reliably.

Website: https://sqry.dev

## Current Capabilities

- Structural queries over symbol kind, language, visibility, names, return types, references, and relations.
- Graph analysis for callers, callees, imports, exports, call paths, cycles, unused symbols, duplicates, impact, semantic diff, and focused subgraphs.
- Edge-backed `returns:<TypeName>` and resolution-aware `resolved_via:<kind>` predicates for supported graph paths.
- Workspace-aware indexing through `.sqry-workspace` registries and VS Code `.code-workspace` `sqry.workspace` blocks. <!-- claim:multi-root-supported test:resolve_logical_workspace_short_circuits_in_documented_order -->
- Daemon-backed shared graph loading through `sqryd` for editor, MCP, and repeated-agent workflows.
- MCP integration for AI assistants. Standalone `sqry-mcp` currently exposes 38 tools; daemon-hosted MCP exposes a 16-tool subset. Use `tools/list`, `sqry-mcp --list-tools`, or `sqry://meta/manifest` as the authoritative catalog.
- LSP and VS Code extension support for editor workflows.

> **Removed in 21.0.0:** the experimental natural-language surface (`sqry ask` CLI command, `sqry_ask` MCP tool, `sqry/ask` LSP request) was removed. Use the structured query and graph commands shown below; see [Removed features](docs/TROUBLESHOOTING.md#removed-features) for migration.

## When To Use sqry

Use sqry when you need structural answers:

```bash
sqry index .
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

Supported release platforms are Linux `x86_64`/`arm64` and macOS `x86_64`/`arm64`.

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

`returns:<TypeName>` uses graph `TypeOf{Return}` edges. `resolved_via:<kind>` accepts `direct`, `type_match`, `binding_plane`, `virtual_dispatch`, `interface_dispatch`, `duck_typed`, `structural`, and `promiscuous_elided`. `framework` filtering is exposed through MCP parameters today; do not rely on `framework:<id>` text grammar unless the current parser documentation and tests in your installed version support it.

See [Advanced Analysis](docs/user-guide/advanced-analysis.md) for graph predicates, snapshot wording, impact analysis, semantic diff, and visualization.

## Workspaces

Use a workspace when one logical project spans several repositories or folders.

```bash
sqry workspace init .
sqry workspace scan .
sqry workspace status . --json
sqry workspace clean . --dry-run
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
- [Revision-Aware Workspaces](docs/user-guide/revision-aware-workspaces.md)
- [Advanced Analysis](docs/user-guide/advanced-analysis.md)
- [Structural Shape Matching](docs/user-guide/shape-match.md)
- [Visualization](docs/user-guide/visualization.md)

## Project Scope

sqry is an MIT-licensed open-source project focused on one product goal: local semantic code search. It is not a hosted search platform, linter, IDE replacement, or general metrics platform.
