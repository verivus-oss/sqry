# sqry Quick Start

**Version**: 20.0.5
**Rust**: 1.94+ (Edition 2024; repository toolchain 1.94.1)

This guide gets you from install to useful semantic queries.

## Install

### Linux And macOS

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
```

Installs `sqry`, `sqry-mcp`, `sqry-lsp`, and `sqryd`. Supported release platforms are Linux `x86_64`/`arm64` and macOS `x86_64`/`arm64`.

Pin a release or enable Cosign verification when needed:

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | \
  bash -s -- --component all --version vX.Y.Z --verify-signatures
```

`--verify-signatures` verifies the `oss-distribute.yml` release bundle identity used by the current installer script. SHA256 verification remains the default integrity check.

### Windows

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
.\install.ps1 -Component all -VerifySignatures
```

The Windows installer downloads the Windows `x86_64` release ZIP and installs all four binaries. Use `.\install.ps1 -Version vX.Y.Z` for a pinned release.

`-VerifySignatures` verifies the `oss-distribute.yml` release bundle identity used by the current installer script. SHA256 verification remains the default integrity check.

### Homebrew

```bash
brew tap verivus-oss/sqry
brew install sqry
```

Homebrew is the current package-manager surface backed by the public release manifest. Use release assets or source builds on platforms where a package-manager surface is not yet published.

### Build From Source

```bash
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo build --workspace
cargo install --path sqry-cli
cargo install --path sqry-mcp
cargo install --path sqry-lsp
cargo install --path sqry-daemon
sqry --version
sqryd --version
```

Requirements:

- Rust `1.94+` with Edition 2024.
- `rust-toolchain.toml` pins `1.94.1`.
- Full builds compile the bundled tree-sitter grammars and language plugins and can require substantial disk space.

## Index A Codebase

```bash
cd /path/to/project
sqry index .
sqry index --status --json .
```

The index writes `.sqry/graph/snapshot.sqry` and related `.sqry/` artifacts. Add `.sqry/` to `.gitignore` unless you intentionally share generated graph state.

Plugin selection:

```bash
sqry index .                         # default fast path
sqry index --include-high-cost .     # all compiled non-default plugins
sqry index --enable-plugin json .    # opt into one plugin
sqry index --force .                 # rebuild after graph-format or semantics upgrades
```

See [Indexing](docs/user-guide/indexing.md) for plugin tiers, cleanup, and artifact taxonomy.

## Search And Query

Pattern search:

```bash
sqry search "parse_.*"
sqry search --exact "main"
sqry search "config" --fuzzy
```

Structural query:

```bash
sqry query "kind:function AND visibility:public"
sqry query "kind:function AND lang:rust"
sqry query "kind:function AND returns:Result"
sqry query "callers:authenticate"
```

Planner-backed query:

```bash
sqry plan-query "kind:function callers:main"
sqry plan-query "kind:function callers:my_read resolved_via:binding_plane"
```

Graph commands:

```bash
sqry graph direct-callers authenticate
sqry graph direct-callees main
sqry graph trace-path main handle_request
sqry cycles
sqry impact authenticate --depth 3
sqry visualize "callers:authenticate" --format mermaid
```

## Natural Language

```bash
sqry ask "find login functions"
sqry ask --dry-run "who calls authenticate"
sqry ask --auto-execute --threshold 0.90 "find public classes"
```

`sqry ask` translates the request into a validated sqry command. It runs immediately only when `--auto-execute` is supplied and the confidence threshold is satisfied. See [Natural Language Queries](docs/user-guide/natural-language.md).

## Workspaces

Use workspaces for multi-root projects:

```bash
sqry workspace init .
sqry workspace scan .
sqry workspace status . --json
sqry workspace clean . --dry-run
```

See [Workspaces](docs/user-guide/workspace.md).

## Daemon

```bash
sqry daemon start
sqry daemon load .
sqry daemon status --json
sqry daemon rebuild . --force
sqry daemon logs --follow
```

See [Daemon Mode](docs/user-guide/daemon.md).

## MCP

```bash
sqry mcp setup --tool claude
sqry mcp setup --tool codex
sqry mcp setup --tool gemini
sqry-mcp --list-tools
```

Standalone MCP exposes the full local tool catalog. Daemon-hosted MCP exposes a smaller daemon-backed subset. Use `tools/list`, `sqry-mcp --list-tools`, or `sqry://meta/manifest` for the authoritative schema. See [MCP Guide](docs/user-guide/mcp.md).

## VS Code

Install the VS Code extension and point it at the same `sqry`/`sqry-lsp` binaries. For shared workspace semantics, see [Workspaces](docs/user-guide/workspace.md); for extension setup, see [sqry-vscode/README.md](sqry-vscode/README.md).

## Next

- [User Guide Index](docs/user-guide/README.md)
- [Advanced Analysis](docs/user-guide/advanced-analysis.md)
- [MCP component docs](sqry-mcp/README.md)
- [CLI component docs](sqry-cli/README.md)
