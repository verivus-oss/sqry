# Troubleshooting Guide

This guide covers common issues and their solutions when using sqry.

## Table of Contents

- [Index Issues](#index-issues)
- [Query Issues](#query-issues)
- [Performance Issues](#performance-issues)
- [MCP Server Issues](#mcp-server-issues)
- [LSP Server Issues](#lsp-server-issues)
- [Cache Issues](#cache-issues)
- [Platform-Specific Issues](#platform-specific-issues)
- [CI/CD Issues](#cicd-issues)

---

## Index Issues

### "No index found" error

**Symptom**: `No index found at <path>. Run 'sqry index' first.`

**Cause**: No index exists at or above the current directory. sqry stores its unified graph index in `.sqry/graph/` (with a legacy `.sqry-index` fallback for discovery).

**Fix**:
```bash
sqry index .
```

### Stale index (missing new files or symbols)

**Symptom**: Recently added files or symbols don't appear in query results.

**Cause**: The index was built before the files were added.

**Fix**:
```bash
# Update the existing index (rebuilds via unified pipeline)
sqry update .

# Or force a full rebuild from scratch
sqry index --force .
```

**Note**: `sqry index .` exits early if an index already exists. Use `sqry update .` for updates or `sqry index --force .` for a full rebuild.

### Index corruption

**Symptom**: Queries fail with deserialization errors or return incorrect results.

**Fix**:
```bash
# Check index health (preview mode, no changes applied)
sqry repair --fix-all --dry-run .

# Force rebuild from scratch
sqry index --force .
```

### Index validation warnings

**Symptom**: Queries report dangling references or orphaned symbols.

**Cause**: Cross-file references point to symbols that no longer exist (renamed or deleted).

**Fix**:
```bash
# Force rebuild clears stale references
sqry index --force .
```

### Large index size

**Symptom**: `.sqry/graph/snapshot.sqry` is hundreds of MB.

**Cause**: Large codebases with many files produce large indexes. Indexes scale with file count and symbol density.

**Mitigation**:
```bash
# sqry respects .gitignore files automatically.
# Add exclusions to .gitignore to reduce index size:
echo "node_modules/" >> .gitignore
echo "target/" >> .gitignore
echo "vendor/" >> .gitignore

# Rebuild after adding exclusions
sqry index --force .
```

---

## Query Issues

### Empty results

**Symptom**: A query returns no results when matches are expected.

**Causes and fixes**:

1. **Stale index**: Rebuild with `sqry update .` or `sqry index --force .`
2. **Wrong predicate syntax**: Check field names (e.g., `kind:function` not `type:function`)
3. **Case sensitivity**: Symbol names are case-sensitive. Use `name~=auth` for substring matching
4. **Wrong path scope**: Verify the query targets the right directory

```bash
# Debug: check what's indexed
sqry graph stats
sqry query "kind:function" --limit 5
```

### "Unknown field" error

**Symptom**: Query parser rejects a predicate.

**Cause**: Using a field name that doesn't exist.

**Valid fields**: `name`, `kind`, `path`/`file`, `lang`/`language`, `parent`, `scope`, `scope.type`, `scope.name`, `scope.parent`, `scope.ancestor`, `text`, `visibility`, `async`, `static`, `callers`, `callees`, `imports`, `exports`, `returns`, `impl`, `references`, `duplicates`, `unused`, `circular`

**Note**: The `repo` field is only available in `sqry workspace query`, not in regular `sqry query`.

### Slow text search

**Symptom**: Queries using `text:` predicate are very slow.

**Cause**: `text:` performs full-text search over symbol bodies and is not indexed.

**Fix**: Use structural predicates instead when possible:
```bash
# Slow (full-text scan, text field requires regex operator)
sqry query "text~=authenticate"

# Fast (indexed lookup)
sqry query "name~=authenticate"
sqry query "callers:authenticate"
```

---

## Performance Issues

### Slow initial index build

**Symptom**: First `sqry index .` takes a long time on a large codebase.

**Cause**: Index builds parse every file in the project.

**Mitigation**:
- Use `.gitignore` to exclude unnecessary directories (sqry respects `.gitignore` automatically)
- Use `sqry update .` for subsequent updates (currently performs a full rebuild via the unified pipeline)

### Graph complexity hangs

**Symptom**: `sqry graph complexity` or `sqry graph call-chain-depth` hangs on large codebases.

**Cause**: The complexity algorithm can hit cycles or exponential paths in large graphs (167MB+ indexes).

**Fix**: Kill the process with `kill -9 <pid>` and use more targeted queries:
```bash
# Instead of whole-graph complexity
sqry graph call-chain-depth "specific_function"

# Or limit the scope
sqry query "kind:function AND path:src/specific_module"
```

### Slow queries after system restart

**Symptom**: First query after a reboot is slow, subsequent queries are fast.

**Cause**: The OS page cache needs to warm up. The index file is memory-mapped.

**Fix**: This is normal. The first query loads the index into memory; subsequent queries reuse it.

---

## MCP Server Issues

### MCP server won't start

**Symptom**: AI assistant can't connect to sqry MCP server.

**Cause**: Usually a configuration or path issue.

**Fix**:
```bash
# Auto-configure for your AI assistant
sqry mcp setup

# Check MCP configuration status
sqry mcp status

# The MCP server binary is sqry-mcp (not a subcommand of sqry)
# AI tools invoke it directly via their MCP config
```

### MCP queries fail with "no index"

**Symptom**: MCP tools return errors about missing index.

**Fix**: Build the index first, then restart the MCP server:
```bash
sqry index .
# Restart your AI assistant's MCP connection
```

### MCP workspace root mismatch

**Symptom**: MCP server rejects paths as "outside of the workspace root".

**Cause**: The MCP server's workspace root doesn't match where you're querying.

**Fix**: Set the workspace root via `SQRY_MCP_WORKSPACE_ROOT` environment variable, or ensure the MCP server is started from the correct directory.

### MCP tools missing or disabled

**Symptom**: Some MCP tools are not available or return "tool not enabled" errors.

**Cause**: MCP feature flags control which tool categories are exposed.

**Fix**: Check and enable the relevant feature flags:
```bash
# All default to true; set to false to disable
export SQRY_MCP_ENABLE_GRAPH=true              # trace_path, subgraph
export SQRY_MCP_ENABLE_EXPORT=true             # export_graph
export SQRY_MCP_ENABLE_CROSS_LANGUAGE=true     # cross_language_edges
export SQRY_MCP_ENABLE_SEMANTIC_DIFF=true      # semantic_diff
export SQRY_MCP_ENABLE_DEPENDENCY_IMPACT=true  # dependency_impact
export SQRY_MCP_ENABLE_SQRY_ASK=true           # sqry_ask
```

### MCP auto-indexing disabled

**Symptom**: MCP server reports "No unified graph found. Auto-indexing is disabled."

**Cause**: Auto-indexing is disabled via `SQRY_AUTO_INDEX=false` or `SQRY_AUTO_INDEX=0`.

**Fix**: Either enable auto-indexing or build the index manually:
```bash
# Option 1: Enable auto-indexing (default)
export SQRY_AUTO_INDEX=true

# Option 2: Build index manually
sqry index .
```

### MCP E2E test timeouts

**Symptom**: MCP E2E tests time out in CI or local runs.

**Cause**: Each test was spawning its own server process. Tests now share a single server process.

**Fix**: This was resolved in commit `6091fafa`. Ensure you're on the latest version.

---

## LSP Server Issues

### LSP server not responding

**Symptom**: Editor shows no hover info, definitions, or references.

**Causes**:
1. **No index**: Build one with `sqry index .`
2. **Wrong startup command**: Ensure your editor runs `sqry lsp --stdio`
3. **Wrong working directory**: The LSP server needs to find the `.sqry/` index

### LSP socket mode connection refused

**Symptom**: Can't connect to `sqry lsp --socket 127.0.0.1:9257`.

**Fix**: Check that the port isn't already in use:
```bash
# Check if port is in use
lsof -i :9257

# Use a different port
sqry lsp --socket 127.0.0.1:9258
```

### Editor configuration

**VS Code**: Use the sqry extension or configure a custom LSP client pointing to `sqry lsp --stdio`.

**Neovim**: Add to your `lspconfig`:
```lua
require('lspconfig').sqry.setup({
  cmd = { "sqry", "lsp", "--stdio" },
})
```

**Helix**: Add to `languages.toml`:
```toml
[[language]]
name = "rust"
language-servers = ["sqry"]

[language-server.sqry]
command = "sqry"
args = ["lsp", "--stdio"]
```

---

## Cache Issues

### Cache taking too much disk space

**Symptom**: `.sqry-cache/` directory is very large.

**Fix**:
```bash
# Check cache stats
sqry cache stats

# Prune old entries
sqry cache prune --days 30

# Or cap to a size limit
sqry cache prune --size 1GB

# Preview before deleting
sqry cache prune --days 7 --dry-run
```

### Cache corruption

**Symptom**: Queries produce unexpected errors related to cached data.

**Fix**:
```bash
# Clear the entire cache
sqry cache clear --confirm

# Rebuild index
sqry index --force .
```

### Custom cache location

By default, the cache lives at `.sqry-cache/` relative to the current working directory (typically the project root when running from the repository root). Override with:

```bash
export SQRY_CACHE_ROOT=/tmp/sqry-cache
sqry index .
```

---

## Platform-Specific Issues

### Windows: Long path errors

**Symptom**: Indexing fails on deeply nested directories.

**Fix**: Enable long path support in Windows:
```powershell
# Run as Administrator
New-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" `
  -Name "LongPathsEnabled" -Value 1 -PropertyType DWORD -Force
```

### Network filesystem performance

**Symptom**: Indexing is extremely slow on network-mounted directories (NFS, SMB/CIFS).

**Cause**: sqry detects network filesystems during configuration initialization. On Linux, detection covers NFS, SMB, CIFS, AFS, and CODA via `statfs` magic numbers. SSHFS and other FUSE-based mounts are not currently detected.

**Fix**: Copy the repository to a local disk for indexing, or use `sqry watch` to keep the index warm:
```bash
# Index locally
cp -r /mnt/network/repo /tmp/local-repo
cd /tmp/local-repo && sqry index .
```

### macOS: "sqry" can't be opened (Gatekeeper)

**Fix**:
```bash
# Remove quarantine attribute
xattr -d com.apple.quarantine $(which sqry)
```

---

## CI/CD Issues

### CI runs out of disk space

**Symptom**: CI build fails with disk space errors.

**Fix**: Clean up before building:
```bash
# Free space on Ubuntu runners
sudo rm -rf /usr/share/dotnet /usr/local/lib/android /opt/ghc

# Or prune after tests
cargo clean -p sqry-lsp
```

### Clippy warnings fail CI

**Symptom**: CI fails on clippy checks even though local clippy passes.

**Cause**: CI enforces `-D warnings` via clippy flags, promoting all warnings to errors.

**Fix**: Run clippy locally with the same flags:
```bash
cargo clippy --all-targets --workspace -- -D warnings
```

### Malformed input tests fail

**Symptom**: FFI safety tests fail for a language plugin.

**Cause**: A tree-sitter grammar returned unexpected data for malformed input.

**Fix**: Ensure your plugin's `build_graph()` handles all tree-sitter parse errors gracefully without panicking. Use `GraphBuildHelper` methods which provide safe defaults.

---

## Common Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `SQRY_CACHE_ROOT` | Cache directory location | `.sqry-cache` |
| `SQRY_MCP_WORKSPACE_ROOT` | MCP server workspace root | Workspace discovery from CWD |
| `SQRY_AUTO_INDEX` | Enable MCP auto-indexing | `true` |
| `SQRY_MCP_ENABLE_GRAPH` | Enable graph tools (trace_path, subgraph) | `true` |
| `SQRY_MCP_ENABLE_EXPORT` | Enable export_graph tool | `true` |
| `SQRY_MCP_ENABLE_CROSS_LANGUAGE` | Enable cross_language_edges tool | `true` |
| `SQRY_MCP_ENABLE_SEMANTIC_DIFF` | Enable semantic_diff tool | `true` |
| `SQRY_MCP_ENABLE_DEPENDENCY_IMPACT` | Enable dependency_impact tool | `true` |
| `SQRY_MCP_ENABLE_SQRY_ASK` | Enable sqry_ask tool | `true` |
| `SQRY_FUZZY_USE_JACCARD` | Fuzzy search mode (`1`=Jaccard, `0`=ratio) | `1` |
| `SQRY_TEST_VERBOSE` | Test verbose logging (`all`, crate names) | Disabled |
| `SQRY_TEST_VERBOSE_LEVEL` | Log level (`trace`, `debug`, `info`, `warn`, `error`, `off`) | `info` |
| `SQRY_TEST_VERBOSE_ARTIFACTS` | Capture test logs to files | Disabled |
| `RUST_BACKTRACE` | Show Rust backtraces on panic | `0` |

For the full list of performance-related variables, see the [Performance Tuning Guide](PERFORMANCE_TUNING.md).

---

## Getting Help

If your issue isn't covered here:

1. Check the [GitHub Issues](https://github.com/verivus-oss/sqry/issues)
2. Run with `RUST_BACKTRACE=1` to get a full stack trace
3. Use verbose test logging for debugging: `SQRY_TEST_VERBOSE=all`
4. Open a new issue with your environment, reproduction steps, and error output
