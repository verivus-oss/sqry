# sqry CLI

**Version**: 20.0.1
**Rust**: 1.94+ (Edition 2024; repository toolchain 1.94.1)

`sqry` is the command-line interface for local semantic code search.

## Install

Recommended binary installers:

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
```

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1 -OutFile install.ps1
.\install.ps1 -Component all
```

Source install:

```bash
cargo install --path sqry-cli
cargo install --path sqry-mcp
cargo install --path sqry-lsp
cargo install --path sqry-daemon
```

The public release assets include `sqry`, `sqry-mcp`, `sqry-lsp`, and `sqryd`. Homebrew is the current package-manager surface backed by the public release manifest.
The installer scripts verify SHA256 checksums by default; optional signature verification checks the `oss-distribute.yml` release bundle identity used by the current installer scripts.

## Core Workflow

```bash
sqry index .
sqry search "parse_.*"
sqry query "kind:function AND visibility:public"
sqry graph direct-callers authenticate
sqry graph trace-path main handle_request
sqry ask --dry-run "who calls authenticate"
```

## Command Families

| Family | Commands |
| --- | --- |
| Search | `search`, top-level pattern shorthand, `hier`, `similar`, `explain` |
| Structural query | `query`, `plan-query` |
| Graph analysis | `graph`, `cycles`, `unused`, `duplicates`, `impact`, `diff`, `subgraph`, `visualize`, `export` |
| Index lifecycle | `index`, `update`, `watch`, `analyze`, `repair`, `cache` |
| Workspace | `workspace init`, `workspace scan`, `workspace add`, `workspace remove`, `workspace query`, `workspace stats`, `workspace status`, `workspace clean` |
| Daemon | `daemon start`, `daemon stop`, `daemon status`, `daemon logs`, `daemon load`, `daemon rebuild`, `daemon reset` |
| Integrations | `lsp`, `mcp setup`, `completions`, `shell`, `batch` |
| Local state | `config`, `alias`, `history`, `insights`, `troubleshoot` |

Use `sqry <command> --help` for the authoritative CLI syntax in your installed binary.

## Indexing

```bash
sqry index .
sqry index --status --json .
sqry index --force .
```

Plugin selection:

```bash
sqry index --include-high-cost .
sqry index --exclude-high-cost .
sqry index --enable-plugin json .
sqry index --disable-plugin json .
```

The default fast path excludes compiled non-default plugins. `json` is high-wall-clock; optional specialty plugins include `apex`, `abap`, `servicenow-xanadu-js`, `servicenow-xml`, `terraform`, `puppet`, and `pulumi` when compiled in.

See [Indexing](../docs/user-guide/indexing.md).

## Workspaces And Daemon

Workspace commands:

```bash
sqry workspace init .
sqry workspace scan .
sqry workspace status . --json
sqry workspace clean . --dry-run
```

Daemon commands:

```bash
sqry daemon start
sqry daemon load .
sqry daemon status --json
sqry daemon rebuild . --force
sqry daemon logs --follow
```

See [Workspaces](../docs/user-guide/workspace.md) and [Daemon Mode](../docs/user-guide/daemon.md).

## MCP Setup

```bash
sqry mcp setup --tool claude
sqry mcp setup --tool codex
sqry mcp setup --tool gemini
sqry-mcp --list-tools
```

See [MCP Guide](../docs/user-guide/mcp.md) and [sqry-mcp/README.md](../sqry-mcp/README.md).

## Related Guides

- [Quick Start](../QUICKSTART.md)
- [User Guide](../docs/user-guide/README.md)
- [Natural Language Queries](../docs/user-guide/natural-language.md)
- [Advanced Analysis](../docs/user-guide/advanced-analysis.md)
