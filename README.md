# sqry

[![MCP Registry](https://img.shields.io/badge/MCP-Registry-blue)](https://registry.modelcontextprotocol.io/servers/io.github.verivus-oss/sqry)
[![crates.io](https://img.shields.io/crates/v/sqry-cli.svg)](https://crates.io/crates/sqry-cli)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

sqry is a semantic code search tool. It parses source code into an AST and builds a graph of symbols and relationships, so you can search by what code **means** rather than just what it says.

Website: https://sqry.dev

## Example

```bash
# Index a codebase (creates .sqry/graph/snapshot.sqry)
sqry index

# Find all public async functions
sqry query "kind:function AND async:true AND visibility:public"

# Who calls this function?
sqry query "callers:authenticate"

# Trace a path between two symbols
sqry graph trace-path main handle_request

# Find circular dependencies
sqry cycles

# Ask in plain English (translates to sqry syntax, then searches)
sqry ask "find all error handling functions"
```

## Why sqry?

If you just need text search, use [ripgrep](https://github.com/BurntSushi/ripgrep) - it's faster and simpler for that job.

sqry is useful when you need to search by code **structure**: finding all callers of a function, tracing execution paths, detecting cycles, or querying by symbol kind, visibility, or return type. These are things text search can't do reliably.

### What sqry is good at

- **Relation queries**: `callers:X`, `callees:X`, `imports:X`, `returns:Result` - exact answers from the call graph
- **Graph analysis**: trace paths between symbols, find cycles, detect unused code
- **Structured queries**: combine predicates with boolean logic (`kind:function AND lang:rust AND async:true`)
- **Cross-language detection**: FFI linking (Rust<>C/C++), HTTP route matching (JS/TS<>Python/Java/Go)
- **AI assistant integration**: MCP server with 34 tools, LSP server for editors

### When NOT to use sqry

- **Simple text search**: Use ripgrep. It's faster for grep-style patterns.
- **Pattern matching on syntax**: Use [ast-grep](https://github.com/ast-grep/ast-grep). It's better for "find this syntax pattern and replace it."
- **Linting or rule enforcement**: Use language-specific linters (clippy, eslint, semgrep).
- **Full IDE features**: Use your editor's built-in language server.
- **Hosted code search**: Use Sourcegraph if you need a web UI and team features.

sqry is focused on one thing: local semantic code search via AST analysis.

## What's New In 10.0.1

- **Workspace-aware multi-repo indexing**: define a logical workspace with a `.sqry-workspace` registry or a VS Code `.code-workspace` `sqry.workspace` block, then query and inspect every source root as one aggregate workspace.
- **VS Code workspace status is authoritative**: the extension now reads the LSP `sqry/workspaceStatus` aggregate, so healthy multi-root workspaces show indexed source roots instead of a false "not indexed" state.
- **Daemon workspace management**: `sqry daemon load` warms a workspace in `sqryd`, `sqry daemon rebuild` triggers an in-place rebuild with optional `--force`, and `sqry-mcp --daemon` / `sqry lsp --daemon` auto-start the daemon when needed.
- **Faster repeated analysis**: derived query caches persist across sessions, graph/MCP/CLI analyses share the `sqry-db` query path, and cold-start queries reuse persisted derived data instead of recomputing every analysis from scratch.
- **MCP robustness improvements**: session-scoped workspace resolution reduces the need to pass `path` on every tool call; duplicate, dependency, cycle, trace, and semantic-diff tools now share stricter frontier and truncation semantics.
- **Release distribution improvements**: Homebrew tap publishing is automated from signed release checksums, and VS Code binary provenance now accepts the current `release-distribute.yml` workflow identity with the previous `oss-distribute.yml` identity retained as a legacy fallback.

## Install

### Windows (recommended)

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
.\install.ps1 -Component all -VerifySignatures
```

This installs `sqry.exe`, `sqry-mcp.exe`, `sqry-lsp.exe`, and `sqryd.exe` into `%LOCALAPPDATA%\Programs\sqry\bin` and adds that directory to the user `PATH`. `-Component all` (the default) installs all four binaries from the Windows release ZIP.
By default the installer resolves the latest GitHub release; use `.\install.ps1 -Version vX.Y.Z` to pin a specific release tag.

`-VerifySignatures` uses Cosign to verify that the downloaded release zip was produced by the official `release-distribute.yml` GitHub Actions workflow for this repository. Older releases signed by `oss-distribute.yml` remain accepted for legacy compatibility. Install `cosign` and ensure it is on `PATH` before using this mode. SHA256 verification remains enabled by default and protects against download corruption; use `-NoChecksum` only for diagnostics.

> **Note**: Windows Defender and enterprise EDR products commonly flag the `irm ... | iex` pattern heuristically, even when the installer script is benign. If you are on a managed machine, use the download-review-run path above or install from release assets/package-manager manifests instead of piping the script directly into `iex`.
>
> If PowerShell execution policy blocks `.\install.ps1` after download, prefer release assets or package-manager manifests rather than weakening machine-wide policy settings.

Convenience shortcut for personal or non-managed machines (installs all four binaries):

```powershell
$script = irm https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.ps1
& ([scriptblock]::Create($script)) -Component all
```

Pinned-release example:

```powershell
.\install.ps1 -Component all -Version v4.8.16 -VerifySignatures
```

Manual fallback:
- download the Windows ZIP release asset (`sqry-<version>-windows-x86_64.zip`)
- extract all contents, including the bundled `.dll` runtime files
- place them in a directory on `PATH`

### macOS and Linux (recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/verivus-oss/sqry/main/scripts/install.sh | bash -s -- --component all
```

Installs `sqry`, `sqry-mcp`, `sqry-lsp`, and `sqryd` into `~/.local/bin` (override with `--install-dir`).
Downloads individual binaries directly from GitHub releases with per-asset SHA256 checksum
verification against `SHA256SUMS.txt`. Add `--verify-signatures` to enforce Cosign bundle
verification (requires `cosign`). Published binaries are available for Linux `x86_64`/`arm64`,
Windows `x86_64`, and macOS Apple Silicon/Intel.

Options: `--component sqry|sqry-mcp|sqry-lsp|sqryd|all`, `--version vX.Y.Z`, `--install-dir DIR`,
`--repo OWNER/REPO`, `--no-checksum`.

### Build from source

```bash
git clone https://github.com/verivus-oss/sqry.git
cd sqry
cargo install --path sqry-cli
cargo install --path sqry-mcp
cargo install --path sqry-lsp
cargo install --path sqry-daemon   # installs sqryd

sqry --version
sqryd --version
```

**Requirements**: Rust 1.90+ (Edition 2024). About 20 GB disk for a full build (37 tree-sitter grammars are compiled from source).

### Package managers

Release packaging configs are generated from signed release checksums for:
- Homebrew (published to the sqry tap and included as a release formula asset)
- Scoop
- Winget
- AUR
- Nix
- Snap

Look for these files in release assets (`homebrew-sqry.rb`, `scoop-sqry.json`, `winget-*.yaml`, `aur-PKGBUILD`, `nix-*.nix`, `snap-snapcraft.yaml`).
Primary install methods provide `sqry`, `sqry-mcp`, `sqry-lsp`, and `sqryd`. The shell installer and the Windows ZIP both support `sqryd` as a component. Raw binaries and `.bundle` files are advanced/manual verification assets.

## Commands

### Indexing Controls

```bash
# Default fast-path index
sqry index

# Include all high-cost plugins
sqry index --include-high-cost

# Force the fast path even if SQRY_INCLUDE_HIGH_COST=1 is set
sqry index --exclude-high-cost

# Include one high-cost plugin explicitly
sqry index --enable-plugin json

# Exclude a plugin explicitly
sqry index --disable-plugin json
```

sqry persists the active plugin set in the unified graph manifest, so later
`query`, `watch`, `diff`, and graph-loader paths reuse the indexed semantics
instead of silently reinterpreting the workspace under current defaults.

Currently, the default fast path excludes these high-cost plugins:
- `json`
- `servicenow-xml`

The same plugin-selection flags also apply to `sqry update` and `sqry watch`.
`--enable-language` and `--disable-language` remain accepted compatibility
aliases for `--enable-plugin` and `--disable-plugin`.

#### Upgrade-rebuild requirement

When sqry's in-format graph semantics change between releases (for example
the v10.0.x Cluster C field-edge source migration that re-sources Go
struct-field `TypeOf{Field}` edges from the new `NodeKind::Property`
nodes), an existing `.sqry/graph/snapshot.sqry` keeps loading but returns
the legacy graph shape until rebuilt. Run `sqry index --force` once after
upgrading across such releases. The release notes for any version that
needs the rebuild call this out explicitly.

### Macro Expansion (Rust)

```bash
# Index with macro boundary analysis
sqry index --enable-macro-expansion

# Include cfg-gated items
sqry index --enable-macro-expansion --cfg 'feature="serde"'

# Search with macro filters
sqry query "kind:function" --cfg-filter 'feature="serde"' --include-generated
sqry query "kind:function" --macro-boundaries
```

### JVM Classpath Analysis

```bash
# Index with automatic wrapper/tool resolution (Gradle: gradlew -> gradle, Maven: mvnw -> mvn)
sqry index --classpath

# Shallow resolution (direct dependencies only)
sqry index --classpath --classpath-depth shallow

# Use a specific classpath file
sqry index --classpath-file classpath.txt

# Restrict discovery to a specific JVM build system in a larger monorepo
sqry index --classpath --build-system gradle
```

- Missing JVM tooling with no cache is treated as an error, not a successful empty external graph.
- Nested JVM build roots are discovered from the repo root, and workspace import edges are scoped to the nearest containing module/root.

### Search and Query

```bash
# On-the-fly search (no index needed, slower)
sqry search "kind:function" src/

# Indexed query (fast, requires sqry index first)
sqry query "kind:class OR kind:struct"
sqry query "kind:function AND visibility:public AND callers:helper"

# Fuzzy search (typo-tolerant)
sqry --fuzzy "patern" .

# Hierarchical search (results grouped by file, useful for RAG)
sqry hier "kind:function visibility:public"
```

### Query Language Reference

`sqry query` (and `sqry plan-query`) accepts a whitespace-separated chain
of *steps*. Each step narrows the result set; the chain must start with a
context-free step (`kind:` or `name:`) and may continue with any number of
filter / relation / traversal steps.

| Step | Example | Meaning |
|------|---------|---------|
| `kind:<NodeKind>` | `kind:function` | Restrict to nodes of the given AST kind (`function`, `method`, `class`, `struct`, `enum`, `interface`, `trait`, `module`, `variable`, `constant`, `type`, `property`, …). |
| `name:<value>` | `name:NeedTags` | **Literal value** is an exact byte-for-byte match against the interned simple **or** qualified name of the node (case-sensitive). **Glob meta** in the value (`*`, `?`, `[`) promotes the pattern to a glob match (e.g. `name:parse_*` matches `parse_expr`, `parse_stmt`). Either way, synthetic placeholder nodes are excluded and there is **no** implicit substring or regex form. See "`name:` contract" below. |
| `visibility:<v>` | `visibility:public` | `public` or `private`. |
| `has:caller` / `has:callee` | `kind:function has:caller` | Existence checks over the call graph. |
| `unused` | `kind:function unused` | Nodes with no inbound use edges. |
| `in:<glob>` | `in:src/api/**/*.rs` | Filter by file-path glob. |
| `scope:<kind>` | `scope:module` | Restrict to a binding-plane scope kind (`module`, `function`, `class`, `namespace`, `trait`, `impl`). |
| `returns:<TypeName>` | `returns:Result` | Functions whose `TypeOf{Return}` edge targets a node whose interned name **equals** `TypeName` (exact, no glob). |
| `callers:<value>` / `callees:<value>` | `callees:visit_*` | Relation predicates; value can be a bare word, a glob, a quoted string, or a sub-query in parentheses. |
| `imports:<value>` / `exports:<value>` | `imports:serde` | Module-graph relation predicates. |
| `implements:<value>` (alias `impl:<value>`) | `kind:class implements:Visitor` | OO interface/trait conformance. |
| `references:<value>` / `references ~= /regex/` | `references ~= /handle_.*/i` | Literal value or regex form (with `i`/`m`/`s` flags). |
| `traverse:<dir>(<edge>,<depth>)` | `kind:function traverse:forward(calls,3)` | Walk `<edge>` for up to `<depth>` hops in `forward` / `reverse` / `both` direction. |

**Name and qualified-name format.** Node names follow per-language
conventions:

- **Simple name** (`entry.name`) — typically the local identifier
  (e.g. `NeedTags`, `parseConfig`).
- **Qualified name** (`entry.qualified_name`) — language-canonical
  fully-qualified form. For Go: `<package>.<TypeName>.<FieldName>`
  (e.g. `main.SelectorSource.NeedTags`); for Rust:
  `<crate>::<module>::<symbol>`; for Java/Kotlin/Scala:
  `<package>.<Class>.<member>`. `name:<X>` matches both fields in
  parallel — it returns a node whose simple name **or** qualified name
  equals `X`.

**`name:` contract (B1_ALIGN, locked).** For **literal** values (no
`*`, `?`, or `[` in the pattern), the planner's `name:<literal>` step
and the top-level CLI shorthand `sqry --exact <literal>` are
contract-bound to return identical sets against any fixture:

```bash
sqry query 'name:NeedTags' .   # planner exact-name lookup (literal)
sqry --exact NeedTags .        # CLI exact-name shorthand
```

Both:

1. Look up the literal in the snapshot string interner.
2. Walk the by-name and by-qualified-name indices for that interned id.
3. Drop synthetic placeholder nodes (Go-plugin `<field:operand.field>`
   shadows; `<ident>@<offset>` per-binding-site Variables; any future
   nodes flagged `NodeMetadata::Synthetic`).
4. Return the deduplicated `NodeId` set.

For values containing glob meta (`*`, `?`, `[`), the planner's `name:`
step promotes to a glob match (still synthetic-filtered); the
top-level CLI `--exact` does **not** accept glob meta and treats every
character as a literal. Use `sqry query 'name:parse_*'` for glob
matching against names; the exact-set-equality contract above does
not apply when the value contains glob meta.

**Surface precedence and synthetic visibility.**

| Surface | Match mode | Synthetic placeholders |
|---------|------------|------------------------|
| `sqry --exact <literal>` (top-level) | Exact byte-for-byte match (literal only; glob meta is treated as literal characters). | Filtered out. |
| `sqry search <pat>` (no flag, default) | Regex match against interned simple **and** qualified names. Invalid regex returns an error; there is no implicit substring fallback. | Not filtered — the regex surface is a literal scan over interned strings. |
| `sqry query 'name:<value>'` (planner) | Exact byte-for-byte match for **literal** values; glob match when the value contains `*`/`?`/`[`. | Filtered out (both literal and glob paths). |
| `sqry query 'name~/<regex>/'` | **Reserved** for a future regex variant (mirrors the existing `references ~= /…/` split). Not yet implemented; users wanting regex name matching today should use `sqry search <regex>` or `references ~= /…/` for incoming references. | n/a |

Only the structured `name:<literal>` prefix and the top-level
`--exact <literal>` shorthand are bound to the exact-set-equality
contract — every other surface in the table has its own match mode and
synthetic-visibility policy as listed.

### Relations

```bash
sqry query "callers:process_data"       # Who calls this?
sqry query "callees:main"               # What does this call?
sqry query "imports:utils"              # Who imports this?
sqry query "returns:Result"             # Functions returning Result

sqry graph direct-callers parse_config  # Direct callers
sqry graph direct-callees main          # Direct callees
sqry graph call-hierarchy parse_config --depth 3
```

### Graph Analysis

```bash
sqry graph trace-path main handle_error    # Execution path between symbols
sqry graph dependency-tree module           # Transitive dependencies
sqry graph cross-language                   # Cross-language relationships
sqry cycles                                 # Circular dependencies
sqry duplicates                             # Duplicate code
sqry unused                                 # Dead code detection
sqry impact parse_config                    # Dependency impact analysis
sqry graph complexity                       # Complexity metrics
sqry diff HEAD~1 HEAD                       # Semantic diff between git refs
```

For very large C++ codebases, pathological single-file graph builds are now
bounded so one oversized translation unit does not pin the entire index run
indefinitely.

### Ambiguous symbol resolution

`sqry impact` and `sqry explain` (and the MCP `dependency_impact` tool)
resolve a bare symbol name against the graph's by-name index. When a
bare name matches more than one indexable node — for example `NeedTags`
matches both the Go `Property` node `main.SelectorSource.NeedTags` and
a local variable of the same name — the command returns a typed
`AmbiguousSymbol` envelope under the stable error code
`sqry::ambiguous_symbol`:

```json
{
  "error": {
    "code": "sqry::ambiguous_symbol",
    "message": "Symbol 'NeedTags' is ambiguous; specify the qualified name",
    "candidates": [
      {
        "qualified_name": "main.SelectorSource.NeedTags",
        "kind": "property",
        "file_path": "main.go",
        "start_line": 12,
        "start_column": 4
      },
      {
        "qualified_name": "main.useSelector.NeedTags",
        "kind": "variable",
        "file_path": "main.go",
        "start_line": 30,
        "start_column": 6
      }
    ],
    "truncated": false
  }
}
```

Pass the qualified name (`sqry impact main.SelectorSource.NeedTags`) to
disambiguate. The candidate list is capped at 20 entries with
`truncated: true` set when the underlying set is larger.

**MCP redaction policy (`minimal` preset, default).** When the same
`AmbiguousSymbol` envelope is delivered through the MCP transport
(e.g. via `dependency_impact`), the `sqry-mcp-redaction` layer
rewrites every per-candidate `file_path` to a workspace-relative form
under the default `minimal` preset, so the absolute repository path
(home directory, machine layout, etc.) is never exposed. The redacted
MCP envelope still surfaces `qualified_name`, `kind`, `start_line`,
and `start_column` per candidate so AI-assistant consumers can
disambiguate. No source excerpt, snippet, or code-context field is
introduced into the envelope. The CLI envelope is unredacted (CLI
users have direct filesystem access already); only the MCP boundary
applies the redaction. For stricter envelopes (filename hashing, no
position info), set `SQRY_REDACTION_PRESET=strict`.

### Natural Language

```bash
sqry ask "What functions handle error recovery?"
sqry ask "Find all public structs" --auto-execute
```

### Export and Visualization

```bash
sqry export --format dot          # Graphviz DOT
sqry export --format mermaid      # Mermaid
sqry export --format d2           # D2 diagrams
sqry export --format json         # JSON

sqry visualize "callers:main" --format mermaid
```

See [docs/user-guide/visualization.md](docs/user-guide/visualization.md) for examples.

### Output Formats

```bash
sqry --preview main                     # Text with context (default 3 lines)
sqry --json --preview 5 "pattern"       # JSON with context
sqry --csv --headers --columns name,file,line pattern > results.csv
```

CSV/TSV output escapes per RFC 4180 and prefixes formula-leading characters unless `--raw-csv` is set.

### Cache Management

```bash
sqry cache stats                    # View cache statistics
sqry cache prune --days 30          # Remove old entries
sqry cache prune --size 1GB         # Cap cache size
sqry cache clear --confirm          # Clear all cached ASTs
sqry cache expand                   # Generate/refresh macro expansion cache (Rust)
sqry cache expand --dry-run         # Preview without writing
```

### Other

```bash
sqry shell                          # Interactive REPL
sqry batch commands.txt             # Batch processing
sqry watch --build                  # Watch mode (auto-update index)
sqry repair --dry-run               # Check for index corruption
sqry --list-languages               # List supported languages
sqry completions bash               # Shell completions
```

## Choosing the Right Command

| Task | Command | Needs Index? |
|------|---------|:---:|
| Quick exploration | `sqry search` | No |
| Find callers/callees | `sqry query` | Yes |
| Trace execution paths | `sqry graph trace-path` | Yes |
| Compare semantic changes across refs | `sqry diff` | Yes |
| Find cycles | `sqry cycles` | Yes |
| Export diagrams | `sqry export` | Yes |

`sqry search` parses files on demand - useful for one-off queries. `sqry query` and `sqry graph` use a prebuilt index and are much faster for repeated use.

## Workspace-Aware Multi-Repo Indexing

For monorepos or saved VS Code workspaces that contain several source roots,
sqry can treat the folders as one logical workspace while preserving per-root
index status.

```bash
sqry workspace init /projects --name "My Workspace"
sqry workspace scan /projects --mode git-roots
sqry workspace add /projects /projects/backend --name backend
sqry workspace query /projects "kind:function AND repo:backend"
sqry workspace stats /projects
sqry workspace status /projects --json --no-cache
```

Discovery modes:
- `index-files` (default): find existing `.sqry/graph/` indexes under the root.
- `git-roots`: find repositories by `.git/` directories.

Use `sqry workspace status` to see each source root as `ok`, `building`,
`missing`, or `error` plus the aggregate workspace verdict. VS Code and the LSP
use the same aggregate status surface, so a healthy multi-root workspace should
show indexed roots in the sqry pane instead of a false "not indexed" state.

See [docs/cli/workspace.md](docs/cli/workspace.md) for `.sqry-workspace` and
`.code-workspace` configuration.

## Language Support

sqry supports **37 languages** through tree-sitter-based plugins. 28 of these have full relation extraction (calls, imports, exports); the remaining 9 have symbol extraction with basic import tracking.

**Full relation support (28)**: C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Perl, PHP, Python, R, Ruby, Rust, Scala, Shell, SQL, Svelte, Swift, TypeScript, Vue, Zig

**Symbol extraction + imports (9)**: JSON, Oracle PL/SQL, Pulumi, Puppet, Salesforce Apex, SAP ABAP, ServiceNow Xanadu, ServiceNow XML, Terraform

Each language plugin lives in its own crate (`sqry-lang-*`) and implements the `GraphBuilder` trait. See [CONTRIBUTING.md](CONTRIBUTING.md) for how to add a new language.

## MCP Server (AI Assistant Integration)

sqry includes an MCP server with 34 JSON-RPC tools for AI assistants (Claude, Codex, Gemini, Cursor, Windsurf):

```bash
# Start MCP server (stdio transport)
sqry-mcp

# Auto-configure for your AI assistant
sqry mcp setup

# Or add to ~/.claude.json manually:
# "mcpServers": { "sqry": { "command": "sqry-mcp", "args": [] } }
```

The MCP server gives AI assistants structured access to the code graph - exact caller/callee lists, path tracing, cycle detection, etc. - rather than relying on text search or embedding similarity.

See [sqry-mcp/README.md](sqry-mcp/README.md) for setup details and tool documentation.
For sensitive repositories, use [sqry-mcp-redaction](sqry-mcp-redaction/README.md) to sanitize MCP responses before sending data to external LLMs.

## LSP Server (Editor Integration)

```bash
sqry lsp --stdio        # Start LSP server (stdio mode)
```

Supports hover, definition, references, call hierarchy, document/workspace symbols, code actions, and 27 custom sqry methods. Works with VS Code, Neovim, Helix, and any LSP 3.17 client. Socket mode available for shared instances.

See `sqry-lsp/` for configuration details.

## VSCode Extension (Preview)

The VS Code extension provides in-editor semantic queries, a "Semantic Results" panel, and CodeLens caller count annotations. <!-- claim:multi-root-supported test:resolve_logical_workspace_short_circuits_in_documented_order --> Multi-root workspaces are supported with per-folder index status and tree data, and cross-repo analysis is opt-in per workspace rather than enabled by default — add a `sqry.workspace` block inside a saved `.code-workspace` file (or commit a `.sqry-workspace` registry under any of your workspace folders) to define `sourceRoots`, `memberFolders`, `exclusions`, and `projectRootMode`. When a `.code-workspace` is open, the extension parses it and forwards a lightweight `{ folders, classification }` hint plus the workspace-file path under `initializationOptions.sqry` to `sqry lsp`. `sqry lsp` then runs its canonical `LogicalWorkspace` resolver in this fixed order (see `sqry-lsp::session::resolve_logical_workspace`): (1) a hand-crafted `LogicalWorkspace` from `initializationOptions.sqry.workspace` (the extension's lightweight hint is detected and skipped), (2) `--index-root <path>/.sqry-workspace`, (3) `<workspace_folder>/.sqry-workspace` walked across every workspace folder, (4) the `.code-workspace` JSON loaded via `from_code_workspace` using `initializationOptions.sqry.workspaceFile`, and (5) anonymous multi-root from the workspace folders as a last resort. The extension also contributes four settings — `sqry.indexRoot`, `sqry.projectRootMode`, `sqry.workspaceFolderExcludes`, `sqry.workspaceClassification` — that govern extension-side runtime behaviour: excludes filter every enumeration loop, and `sqry.workspaceClassification` is the user-editable surface for the inline `sqry.workspace` block. <!-- claim:configurable-cross-repo test:resolve_logical_workspace_short_circuits_in_documented_order --> See [docs/cli/workspace.md](docs/cli/workspace.md) for the full configuration surface, including the CLI parity flag (`--workspace <PATH>` / `SQRY_WORKSPACE_FILE`). Source is in `sqry-vscode/`.

```bash
cd sqry-vscode && npm install && npm run compile
# Launch via F5 in VS Code
```

## Configuration

sqry stores configuration in `.sqry/graph/config/config.json`, created automatically with sensible defaults on first use.

```bash
# View configuration
sqry config show

# Common environment variable overrides
export SQRY_CACHE_ROOT=/mnt/ssd/.sqry-cache    # Custom cache location
export SQRY_CACHE_MAX_BYTES=524288000           # 500 MB cache limit
export SQRY_CACHE_DISABLE_PERSIST=1             # Memory-only caching
```

See [docs/PERFORMANCE_TUNING.md](docs/PERFORMANCE_TUNING.md) for all tuning options and [CONFIGURATION_TUNING_GUIDE.md](CONFIGURATION_TUNING_GUIDE.md) for the full reference.

### Index Flags

```bash
sqry index . --no-incremental       # Force full rebuild
sqry index --validate=fail          # Strict validation (exit 2 on corruption)
sqry update --validate=fail --auto-rebuild  # Auto-rebuild on corruption
```

### .sqryignore

Exclude files from indexing using gitignore syntax:

```bash
echo "node_modules/" >> .sqryignore
echo "target/" >> .sqryignore
sqry index
```

sqry also skips common dependency, generated, build-output, editor-cache, and
CI-runner roots by default: `.git`, `.hg`, `.svn`, `.cache`, `.next`, `.nuxt`,
`.sqry`, `.turbo`, `.venv`, `__pycache__`, `_actions`, `_update`, `_work`,
`build`, `dist`, `node_modules`, `target`, `vendor`, `venv`, and directories
whose names start with `externals.`. This keeps editor-triggered indexing from
walking third-party or generated trees when ignore files are absent. If a
repository intentionally stores first-party code in those directories, run with
`SQRY_INCLUDE_DEFAULT_EXCLUDED_DIRS=1`.

## Daemon Mode

`sqryd` is a background daemon that keeps your code graph in memory across multiple CLI, LSP, and MCP invocations. Instead of parsing and indexing from scratch on every `sqry query` or AI assistant request, `sqryd` keeps the graph hot and rebuilds it incrementally when source files change.

```bash
# Start the daemon (background process, keeps graph in memory)
sqry daemon start
sqry daemon load /path/to/project

# Check status — shows version, uptime, memory, workspaces
sqry daemon status

# Rebuild a loaded workspace in place
sqry daemon rebuild /path/to/project --timeout 1800

# Stop the daemon
sqry daemon stop

# Tail the daemon log (requires log_file set via daemon.toml or SQRY_DAEMON_LOG_FILE)
sqry daemon logs --follow
```

Example `sqry daemon status` output:

```
sqryd v10.0.1 -- uptime 2h 14m

Memory: 391 MB / 2048 MB  (peak: 418 MB)

Workspaces (2 loaded):
  ~/projects/sqry      304 MB  (peak: 318 MB)  [Loaded]
  ~/projects/myapp      87 MB  (peak:  91 MB)  [Loaded]
```

The `peak` value per workspace shows the high-water mark: the maximum resident memory since that workspace was loaded or last evicted. It persists across incremental rebuilds but resets when the workspace is unloaded or evicted. The aggregate `Memory` peak shows the highest total across all workspaces. A peak close to the memory limit signals you are near the LRU eviction threshold.

### Daemon-backed LSP and MCP

Pass `--daemon` to `sqry-mcp` or `sqry lsp` to route all tool calls through the running daemon instead of loading the graph in-process:

```bash
sqry-mcp --daemon                          # connect to default socket
sqry-mcp --daemon --daemon-socket /tmp/sqryd.sock  # custom socket path
sqry lsp --daemon                          # LSP backed by daemon
```

If no daemon is running when `--daemon` is specified, `sqry-mcp` and `sqry lsp` will auto-start one. Binary resolution order: `SQRYD_PATH` env var, then a `sqryd` sibling next to the running binary, then `sqryd` on `PATH`.

### Configuration

The daemon reads `~/.config/sqry/daemon.toml` at startup (XDG; on macOS: `~/Library/Application Support/sqry/daemon.toml`). Key fields:

```toml
memory_limit_mb       = 2048   # evict LRU workspaces above this threshold (MB)
idle_timeout_minutes  = 30     # unload workspace after N minutes of inactivity
log_max_size_mb       = 50     # rotate log file at this size (MB)
log_keep_rotations    = 5      # number of rotated log files to retain

[socket]
path      = ""    # Unix socket path (default: $XDG_RUNTIME_DIR/sqry/sqryd.sock,
                  #   fallback: $TMPDIR/sqry-<uid>/sqryd.sock, then /tmp/sqry-<uid>/sqryd.sock)
pipe_name = ""    # Windows named pipe name (default: sqry → \\.\pipe\sqry)
```

Selected environment-variable overrides:
- `SQRY_DAEMON_MEMORY_MB` — override `memory_limit_mb`
- `SQRY_DAEMON_SOCKET` — override `socket.path`
- `SQRY_DAEMON_LOG_LEVEL` — override log level (`info`, `debug`, `warn`, `error`)

A daemon restart is required for any configuration change to take effect, whether editing `daemon.toml` or changing environment variables.

See [docs/cli/daemon.md](docs/cli/daemon.md) for the full CLI reference, all flags, exit codes, and tuning guidance.

## How It Works

sqry builds a code graph in 5 passes:

1. **AST parsing** - tree-sitter parses source files into syntax trees
2. **Symbol extraction** - nodes (functions, classes, etc.) and structural edges (defines, contains)
3. **Enrichment** - visibility, types, async markers
4. **Intra-file edges** - calls, references within each file
5. **Cross-file edges** - import resolution, FFI linking, HTTP route matching

The resulting graph is persisted to `.sqry/graph/snapshot.sqry` and loaded for queries. Graph queries use precomputed analyses (SCCs, condensation DAGs, 2-hop labels) for fast cycle detection, path finding, and reachability.

### Architecture

sqry uses arena-based storage with CSR (Compressed Sparse Row) adjacency for cache-friendly O(1) traversal, generational indices for safety, MVCC for concurrent reads, and string interning to reduce memory. Parallelism uses [rayon](https://github.com/rayon-rs/rayon); locks use [parking_lot](https://github.com/Amanieu/parking-lot).

The **binding plane** is a first-class derived layer (Phase 4e, V9 snapshot) that computes scope/alias/shadow tables from the structural edges emitted by language plugins, then exposes witness-bearing resolution via `BindingPlane<'g>`. The `sqry graph resolve <symbol> [--explain]` command is the CLI proof point for this layer.

### Performance Notes

These numbers are from sqry's own codebase (~384K nodes, ~1.3M edges). Your results will vary depending on codebase size, language mix, and hardware.

- **Indexed queries**: ~4ms with warm cache (vs ~452ms cold)
- **Cycle check**: <1s (precomputed SCC lookup)
- **Path finding**: <1s typical (condensation DAG pruning)
- **Indexing throughput**: varies by language (JavaScript ~760K LOC/s, C++ ~1.1M LOC/s, Python ~500K LOC/s on the test machine)

## Project Structure

```
sqry/
├── sqry-core/              # Core library (graph, symbols, search, plugin system)
├── sqry-cli/               # CLI binary ('sqry')
├── sqry-lsp/               # LSP server
├── sqry-mcp/               # MCP server (34 tools for AI assistants)
├── sqry-daemon/            # Daemon binary + library (sqryd)
├── sqry-daemon-protocol/   # Wire types and framing (free functions, envelope types)
├── sqry-daemon-client/     # Client library (shim mode, management API)
├── sqry-db/                # Derived analysis DB + query planner (Phase 3)
├── sqry-classpath/         # JVM classpath analysis (bytecode, Gradle/Maven/Bazel/sbt)
├── sqry-nl/                # Natural language query translation
├── sqry-plugin-registry/   # Plugin registration
├── sqry-mcp-redaction/     # MCP response redaction
├── sqry-lang-*/            # 37 language plugins
├── sqry-lang-support/      # Plugin infrastructure
├── sqry-tree-sitter-support/ # Tree-sitter helpers
├── sqry-test-support/      # Test infrastructure
└── crates/tree-sitter-*    # Vendored tree-sitter grammars
```

## Comparison

| Feature | sqry | ripgrep | ast-grep | Sourcegraph |
|---------|------|---------|----------|-------------|
| Text search | Yes (with fallback) | Yes | No | Yes |
| AST awareness | Yes | No | Yes | Yes |
| Relation queries | Yes (28 langs) | No | No | Yes |
| Local/offline | Yes | Yes | Yes | No |
| MCP server | Yes (34 tools) | No | No | No |
| LSP server | Yes | No | No | Yes |
| Cost | Free (MIT) | Free | Free | Paid |

This is a rough comparison; each tool has different strengths. ripgrep is faster than sqry for plain text search. ast-grep is better for syntax pattern matching. Sourcegraph offers team features sqry doesn't. sqry's strength is local graph-based queries (callers, cycles, paths).

## Contributing

We welcome contributions of all sizes - from typo fixes to new language plugins. See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

```bash
cargo build --workspace                              # Build
cargo test --workspace                               # Tests
cargo fmt --all                                      # Format
cargo clippy --all-targets --workspace -- -D warnings  # Lint
```

## Documentation

- [Quick Start Guide](QUICKSTART.md) - Get started in 5 minutes
- [Usage Examples](docs/USAGE_EXAMPLES.md) - Real-world examples
- [Feature List](docs/FEATURE_LIST.md) - Complete feature reference
- [Schema Reference](docs/SCHEMA.md) - Query syntax and metadata keys
- [Performance Tuning](docs/PERFORMANCE_TUNING.md) - Optimization guide
- [CLI Documentation](sqry-cli/README.md) - Full CLI reference
- [Daemon CLI Reference](docs/cli/daemon.md) - `sqry daemon` subcommands, flags, exit codes, and memory tuning
- [MCP Server](sqry-mcp/README.md) - AI assistant integration
- [Troubleshooting](docs/TROUBLESHOOTING.md) - Common issues and solutions

## Acknowledgments

sqry depends on excellent open-source projects:

- [tree-sitter](https://github.com/tree-sitter/tree-sitter) - Incremental parsing (and the many grammar maintainers)
- [ripgrep](https://github.com/BurntSushi/ripgrep) - Text search engine (used as a library for fallback search)
- [rayon](https://github.com/rayon-rs/rayon) - Data parallelism
- [serde](https://github.com/serde-rs/serde) - Serialization
- [clap](https://github.com/clap-rs/clap) - CLI argument parsing
- [tokio](https://github.com/tokio-rs/tokio) - Async runtime (LSP/MCP servers)

## License

MIT - see [LICENSE](LICENSE)

---

Developed by [Verivus](https://sqry.dev)
