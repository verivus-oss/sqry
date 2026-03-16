# sqry VS Code Extension

Semantic code search for **35 languages**. Navigate call graphs, inheritance hierarchies, FFI boundaries, imports/exports via unified graph architecture.

## Features

- **Semantic Search**: Fuzzy, typo-tolerant search that understands code structure
- **Structured Queries**: Boolean logic, filters (async, visibility, return types)
- **Relationship Navigation**: Find callers, callees, imports, exports, inheritance
- **OOP Support**: Class inheritance, interface implementations, trait bounds
- **FFI Detection**: Cross-language calls (JNI, CGo, ctypes, WebAssembly, native addons)
- **CodeLens Integration**: Inline caller counts and quick actions
- **Results Panel**: Dedicated view for symbols and text matches
- **Analysis Panels**: Duplicate code, circular dependencies, unused symbols
- **Cross-Language Panel**: Cross-language call/import edges across languages
- **Auto-Indexing**: Automatic workspace indexing on open
- **Real-time Progress**: Detailed progress during indexing

## Supported Languages (35)

**Tier-1 (Full Graph Support)**:
Rust, JavaScript, TypeScript, Python, Go, Java, C, C++, C#

**Tier-2 (Extended Support)**:
Kotlin, Scala, Ruby, Swift, PHP, Lua, Perl, Elixir, Haskell, R, Dart, Zig, Groovy, Shell/Bash

**Tier-3 (Basic Support)**:
HTML, CSS, SQL, Oracle PL/SQL, Terraform/HCL, Puppet, Pulumi, SAP ABAP, Salesforce Apex, ServiceNow, Vue, Svelte

## What's New in v4.10.1

- Version-aligned with sqry `v4.10.1` (CLI/LSP/MCP).
- Release and documentation links aligned to current OSS workflow.
- Extension packaging and verification flow aligned with current release assets.

## Quick Start

1. **Install sqry CLI** (prerequisite):
   ```bash
   cargo install sqry-cli
   sqry --version  # Verify: 4.10.1+
   ```

2. **Install extension** from VSIX:
   ```bash
   code --install-extension sqry-vscode-4.10.1.vsix
   ```

3. **Index your project**:
   ```
   Command Palette (Ctrl/Cmd+Shift+P) → Sqry: Index Workspace
   ```

4. **Search**:
   ```
   Command Palette → Sqry: Search Workspace... → "authenticate"
   ```

## Documentation

- **[User Guide](USER_GUIDE.md)** - Complete installation, setup, and usage guide
- **[Troubleshooting](TROUBLESHOOTING.md)** - Common issues and solutions
- **[Test Coverage](tests/TEST_COVERAGE.md)** - Testing documentation
- **[Changelog](CHANGELOG.md)** - Version history

## Configuration

Configure the extension via VSCode settings:

```json
{
  "sqry.path": "sqry",                  // Path to sqry binary
  "sqry.limit": 200,                    // Max results per query
  "sqry.timeoutMs": 15000,              // Timeout for search/query (15s)
  "sqry.indexTimeoutMs": 300000,        // Timeout for index rebuilds (5 min)
  "sqry.autoIndexOnOpen": "prompt",     // "always", "prompt", or "never"
  "sqry.codeLens.enabled": true         // Enable CodeLens caller counts
}
```

### Timeout Configuration

- **`sqry.timeoutMs`** (default: 15s) - For quick operations (search, relations)
- **`sqry.indexTimeoutMs`** (default: 5 min) - For index rebuilds

**For large codebases** (10,000+ symbols):
```json
{
  "sqry.indexTimeoutMs": 600000  // 10 minutes
}
```

## Example Queries

**Find all async functions**:
```
kind:function AND async:true
```

**Find error handlers**:
```
kind:function AND name~=/error|catch|handle/
```

**Find public exports**:
```
visibility:public AND kind:function
```

**Find FFI calls**:
```
kind:ffi_call
```

**Find callers of a function**:
```
Right-click on function → Find Callers
```

See [User Guide](USER_GUIDE.md#query-syntax) for complete query syntax.

## Additional Commands

The extension also provides utility commands:

- **Sqry: Refresh Index Stats** (`sqry.refreshStats`)
- **Sqry: Clear Results** (`sqry.clearResults`)

## Graph Edge Types

sqry builds a unified code graph with 20+ edge types:

| Edge Type | Description |
|-----------|-------------|
| `Calls` | Function/method invocations (with argument count, async flag) |
| `Imports` | Module/package imports (with alias, wildcard support) |
| `Exports` | Public API exports (with kind, alias) |
| `Inherits` | Class inheritance, struct embedding |
| `Implements` | Interface/trait implementations |
| `Defines` | Symbol definitions |
| `Contains` | Structural containment |
| `References` | Symbol references |
| `TypeOf` | Type annotations |
| `FfiCall` | Cross-language FFI calls |
| `HttpRequest` | HTTP endpoint calls (fetch, axios, etc.) |
| `GrpcCall` | gRPC service calls |
| `WebAssemblyCall` | WebAssembly interop |
| `DbQuery` | Database query detection |
| `MessageQueue` | Message queue publish/subscribe |
| `WebSocket` | WebSocket connections |
| `GraphQLOperation` | GraphQL queries/mutations |

## Progress Indicators

During indexing, sqry displays:
- **Current file** being processed
- **Percentage** complete (0% → 100%)
- **Total symbols** and duration on completion

### Requirements

- sqry CLI version 4.10.1 or later
- VSCode 1.85.0 or later

---

## For Developers

### Building

```bash
npm install
npm run compile
```

### Testing

```bash
# Unit tests (fast)
npm run test:unit

# Integration tests (requires VS Code)
npm run test:integration

# Default (unit tests)
npm test
```

### Packaging

```bash
npx @vscode/vsce package
# Produces: sqry-vscode-4.10.1.vsix
```

### Diagnostics

Enable debug logging:
- Extension logs: `View → Output → sqry`
- CLI logs: Set `RUST_LOG=debug` environment variable

---

## Support & Feedback

- [User Guide](USER_GUIDE.md) - Complete documentation
- [Troubleshooting](TROUBLESHOOTING.md) - Common issues
- [GitHub Discussions](https://github.com/verivus-oss/sqry/discussions)
- [Report an Issue](https://github.com/verivus-oss/sqry/issues)

---

## License

MIT - See root LICENSE file

**Last Updated**: 2026-03-15
**Version**: 4.10.1
**Requires**: sqry CLI 4.10.1+
