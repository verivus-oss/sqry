# sqry VS Code Extension

Semantic code search for sqry-indexed workspaces. Navigate call graphs, inheritance hierarchies, FFI boundaries, imports/exports — powered by unified graph architecture with AST-based analysis.

## Features

### Search & Navigation
- **Semantic Search**: Find symbols by name across your entire workspace (`Ctrl+Alt+S`)
- **Structured Queries**: Boolean filters by kind, visibility, language, async, return type (`Ctrl+Alt+Q`)
- **Relationship Navigation**: Find callers, callees, imports, exports, inheritance (`Ctrl+Alt+R`)
- **Search History**: Recall and re-run previous queries with MRU list

### Code Intelligence
- **CodeLens**: Inline caller and callee counts above functions/methods — click to explore (`sqry.codeLens.segments` configurable)
- **Hover Integration**: Caller/callee counts in editor hover tooltips
- **Problems Panel**: Unused code, circular dependencies, and duplicates surfaced as native VS Code diagnostics
- **Inline Unused Code Fading**: Unused symbols rendered as dimmed text (via `DiagnosticTag.Unnecessary`)
- **Quick Fixes**: Code actions for "Show callers", "Show cycle path", "Navigate to duplicate"

### Visualization
- **Call Graph Webview**: Interactive SVG visualization of callers/callees with pan/zoom, search, and export
- **Dependency Graph Webview**: Cross-language relationship visualization
- **Results Panel**: Dedicated sidebar with symbol and text match results
- **Analysis Panels**: Duplicate code, circular dependencies, unused symbols (lazy-loaded)
- **Cross-Language Panel**: Cross-language call/import edges with per-language-pair counts

### Workspace Management
- **Status Bar**: Persistent index health indicator (Ready/Stale/Building/No Index/Error) — click to act
- **Auto-Indexing**: Automatic workspace indexing on open (configurable: always/prompt/never)
- **Auto-Index on Save**: Optional debounced rebuild after file saves (`sqry.autoIndexOnSave`)
- **Multi-Root Workspace**: Per-root index status, targeting, aggregate status bar, and `.code-workspace` `sqry.workspace` classification
- **Authoritative Workspace Status**: Uses the LSP `sqry/workspaceStatus` aggregate so indexed multi-root workspaces do not appear as a false single-folder "not indexed" state
- **Real-time Progress**: Detailed progress during indexing with auto-refresh on completion

### Developer Experience
- **Keyboard Shortcuts**: Ctrl+Alt+S (search), Ctrl+Alt+Q (query), Ctrl+Alt+R (references), Ctrl+Alt+I (index)
- **Result Filtering**: Filter results by language and symbol kind
- **Result Sorting**: Sort by name, file path, kind, or line number
- **Export Results**: Export as JSON, Markdown, or CSV
- **Getting Started Walkthrough**: 5-step onboarding for new users
- **Auto-Download**: Automatically downloads the sqry binary from GitHub releases with checksum and Sigstore provenance verification

## Supported Languages

The default extension indexing path enables the 35 standard language plugins.
The full sqry distribution includes 37 tree-sitter plugins; high-cost plugins
such as JSON and ServiceNow XML can be enabled from the CLI when needed.

All languages use the same unified GraphBuilder architecture with AST-based semantic analysis:

Rust, JavaScript, TypeScript, Python, Go, Java, C, C++, C#, Kotlin, Scala, Ruby, Swift, PHP, Lua, Perl, Elixir, Haskell, R, Dart, Zig, Groovy, Shell/Bash, HTML, CSS, SQL, Oracle PL/SQL, Terraform/HCL, Puppet, Pulumi, SAP ABAP, Salesforce Apex, ServiceNow, Vue, Svelte

## Quick Start

1. **Install the extension** — it will offer to download the sqry binary automatically. Or install manually:
   ```bash
   cargo install sqry-cli
   ```

2. **Index your project**:
   ```
   Command Palette (Ctrl/Cmd+Shift+P) → Sqry: Index Workspace
   ```

3. **Search**:
   ```
   Ctrl+Alt+S → type a symbol name
   ```

4. **Explore callers**: Open any source file — CodeLens shows caller/callee counts above functions. Click to navigate.

## Commands

| Command | Shortcut | Description |
|---------|----------|-------------|
| Sqry: Search Workspace | `Ctrl+Alt+S` | Find symbols by name |
| Sqry: Query | `Ctrl+Alt+Q` | Run structured semantic query |
| Sqry: Find Semantic References | `Ctrl+Alt+R` | Find callers/callees/references for symbol under cursor |
| Sqry: Index Workspace | `Ctrl+Alt+I` | Build or rebuild the semantic index |
| Sqry: Search History | — | Recall and re-run previous queries |
| Sqry: Show Call Graph | — | Open interactive call graph visualization |
| Sqry: Show Dependencies | — | Open cross-language dependency graph |
| Sqry: Scan Workspace for Problems | — | Full workspace diagnostic scan |
| Sqry: Filter Results | — | Filter search results by language/kind |
| Sqry: Sort Results | — | Sort search results |
| Sqry: Export Results | — | Export results as JSON/Markdown/CSV |
| Sqry: Restart Language Server | — | Stop and restart the LSP server |
| Sqry: Refresh Index Stats | — | Refresh the index statistics display |
| Sqry: Clear Results | — | Clear the search results panel |

## Configuration

```json
{
  "sqry.path": "sqry",                      // Path to sqry binary
  "sqry.limit": 200,                        // Max results per query
  "sqry.timeoutMs": 15000,                  // Timeout for search/query (15s)
  "sqry.indexTimeoutMs": 300000,            // Timeout for index rebuilds (5 min)
  "sqry.autoIndexOnOpen": "prompt",         // "always", "prompt", or "never"
  "sqry.autoIndexOnSave": "never",          // "never" or "debounced" (30s delay)
  "sqry.autoDownload": true,                // Auto-download binary from GitHub
  "sqry.indexRoot": "",                     // Optional index root override
  "sqry.projectRootMode": "gitRoot",        // "gitRoot", "folder", or "explicit"
  "sqry.workspaceFolderExcludes": [],       // Workspace folders to skip
  "sqry.workspaceClassification": null,     // .code-workspace source/member classification
  "sqry.codeLens.enabled": true,            // Enable CodeLens annotations
  "sqry.codeLens.segments": ["callers", "callees"],  // Which counts to show
  "sqry.diagnostics.enabled": true,         // Enable Problems panel integration
  "sqry.diagnostics.unusedCode": true,      // Show unused code as faded text
  "sqry.hover.enabled": true                // Show sqry info in hover tooltips
}
```

For saved multi-root workspaces, add a `sqry.workspace` block to the
`.code-workspace` file to opt into cross-repo analysis:

```jsonc
{
  "folders": [
    { "path": "services/auth" },
    { "path": "services/billing" },
    { "path": "docs" }
  ],
  "sqry.workspace": {
    "sourceRoots": ["services/auth", "services/billing"],
    "memberFolders": ["docs"],
    "exclusions": ["vendor"],
    "projectRootMode": "gitRoot"
  }
}
```

**For large codebases** (10,000+ symbols), increase the index timeout:
```json
{
  "sqry.indexTimeoutMs": 600000
}
```

## Example Queries

| Query | What it finds |
|-------|---------------|
| `kind:function AND async:true` | All async functions |
| `kind:function AND name~=/error\|catch/` | Error handlers |
| `visibility:public AND kind:function` | Public API functions |
| `callers:handleRequest` | Who calls handleRequest? |
| `callees:main` | What does main call? |
| `returns:Result` | Functions returning Result |
| `kind:class AND lang:rust` | Rust structs/classes |

See [User Guide](USER_GUIDE.md#query-syntax) for complete query syntax.

## Graph Edge Types

sqry builds a unified code graph with 26 edge types:

| Category | Edge Types |
|----------|-----------|
| **Structural** | `Defines`, `Contains` |
| **References** | `Calls`, `References`, `Imports`, `Exports`, `TypeOf` |
| **OOP** | `Inherits`, `Implements` |
| **Cross-Language** | `FfiCall`, `HttpRequest`, `GrpcCall`, `WebAssemblyCall`, `DbQuery` |
| **Extended** | `MessageQueue`, `WebSocket`, `GraphQLOperation`, `ProcessExec`, `FileIpc`, `ProtocolCall` |

## Documentation

- **[User Guide](USER_GUIDE.md)** — Complete installation, setup, and usage guide
- **[Troubleshooting](TROUBLESHOOTING.md)** — Common issues and solutions
- **[Changelog](CHANGELOG.md)** — Version history

## Requirements

- VS Code 1.85.0 or later
- sqry binary (auto-downloaded or manually installed)

---

## For Developers

### Building

```bash
npm install
npm run compile
```

### Testing

```bash
npm run test:unit # Unit tests
npm run test:integration  # Integration tests (requires VS Code)
```

### Packaging

```bash
npx @vscode/vsce package
```

### Diagnostics

- Extension logs: `View → Output → sqry`
- CLI debug logs: Set `RUST_LOG=debug` environment variable

---

## Support & Feedback

- [GitHub Discussions](https://github.com/verivus-oss/sqry/discussions)
- [Report an Issue](https://github.com/verivus-oss/sqry/issues)

---

## License

MIT — See root LICENSE file

---

**Version**: 12.1.2
**Last Updated**: 2026-05-03
