# sqry MCP Server - Troubleshooting Guide - by Verivus

**Version**: 15.0.1
**Last Updated**: 2026-05-11

Quick solutions to common issues with the sqry MCP server.

---

## Quick Diagnostics

**Before troubleshooting**, run these checks:

1. **Check sqry CLI**:
   ```bash
   sqry --version
   # Should output: sqry 15.0.1 or later
   ```

2. **Check MCP server binary**:
   ```bash
   ls -l /path/to/sqry/target/release/sqry-mcp
   # Should exist and be executable
   ```

3. **Test server directly**:
   ```bash
   /path/to/sqry-mcp --list-tools
   # Should list 34 tools
   ```

4. **Check index**:
   ```bash
   cd /your/project
   ls -la .sqry/graph
   # Should exist
   ```

---

## Installation Issues

### Binary Not Found

**Symptom**: "sqry-mcp: command not found" or "No such file or directory"

**Solutions**:

1. **Build the server**:
   ```bash
   cd /path/to/sqry
   cargo build --release -p sqry-mcp
   ls -l target/release/sqry-mcp  # Verify
   ```

2. **Use absolute path in config**:
   ```json
   {
     "command": "/absolute/path/to/sqry/target/release/sqry-mcp"
   }
   ```

3. **Check permissions**:
   ```bash
   chmod +x /path/to/sqry/target/release/sqry-mcp
   ```

### Build Fails

**Symptom**: Cargo build fails with errors

**Solutions**:

1. **Update Rust**:
   ```bash
   rustup update
   rustc --version  # Should be 1.90+
   ```

2. **Clean and rebuild**:
   ```bash
   cargo clean
   cargo build --release -p sqry-mcp
   ```

3. **Check dependencies**:
   ```bash
   cargo check -p sqry-mcp
   ```

### sqry CLI Not Found

**Symptom**: MCP server fails with "sqry not found"

**Solutions**:

1. **Install sqry CLI**:
   ```bash
   cargo install --path sqry-cli
   ```

2. **Verify installation**:
   ```bash
   which sqry      # Linux/Mac
   where sqry      # Windows
   ```

3. **Add to PATH**:
   ```bash
   # Add to ~/.bashrc or ~/.zshrc
   export PATH="$HOME/.cargo/bin:$PATH"
   ```

---

## Connection Issues

### AI Assistant Not Connecting

**Symptom**: MCP server doesn't appear in AI assistant

**Claude Desktop Solutions**:

1. **Check config file location**:
   - macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
   - Linux: `~/.config/Claude/claude_desktop_config.json`
   - Windows: `%APPDATA%\Claude\claude_desktop_config.json`

2. **Validate JSON syntax**:
   ```bash
   # Use a JSON validator
   cat ~/Library/Application\ Support/Claude/claude_desktop_config.json | jq .
   # Should parse without errors
   ```

3. **Use absolute paths**:
   ```json
   {
     "command": "/Users/username/sqry/target/release/sqry-mcp"
     // NOT: "~/sqry/target/release/sqry-mcp"
     // NOT: "./target/release/sqry-mcp"
   }
   ```

4. **Restart completely**:
   - Quit Claude Desktop (Cmd+Q / Alt+F4)
   - Wait 5 seconds
   - Reopen

5. **Check logs**:
   ```bash
   # macOS
   tail -f ~/Library/Logs/Claude/mcp*.log

   # Linux
   tail -f ~/.config/Claude/logs/mcp*.log
   ```

**Windsurf Solutions**:

1. **Reload Windsurf**:
   - Ctrl/Cmd+Shift+P → Reload Window

2. **Check settings syntax**:
   - Open MCP settings
   - Verify JSON is valid

3. **Check developer console**:
   - View → Toggle Developer Tools
   - Look for MCP-related errors

**Cursor Solutions**:

1. **Enable MCP**:
   - Settings → Features → Enable MCP Protocol

2. **Verify config location**:
   - `~/.cursor/mcp_settings.json`

3. **Restart Cursor completely**

**Codex CLI Solutions**:

1. **Check Codex config location**:
   - `~/.codex/config.toml`

2. **Auto-configure sqry entry**:
   ```bash
   sqry mcp setup --tool codex
   sqry mcp status
   ```

3. **Verify CWD-based discovery**:
   - Start Codex from the target repository root
   - Ensure the repo has an index (`sqry index .`)

**Gemini CLI Solutions**:

1. **Check Gemini config location**:
   - `~/.gemini/settings.json`

2. **Auto-configure sqry entry**:
   ```bash
   sqry mcp setup --tool gemini
   sqry mcp status
   ```

3. **Verify CWD-based discovery**:
   - Start Gemini from the target repository root
   - Ensure the repo has an index (`sqry index .`)

### Server Starts But No Tools

**Symptom**: Connection succeeds but no tools available

**Solutions**:

1. **Test server directly**:
   ```bash
   /path/to/sqry-mcp --list-tools
   # Should return 34 tools
   ```

2. **Check server version**:
   ```bash
   /path/to/sqry-mcp --version 2>&1 || echo "No version flag"
   ```

3. **Rebuild server**:
   ```bash
   cargo clean -p sqry-mcp && cargo build --release -p sqry-mcp
   ```

4. **Check for errors in logs**

---

## Index Issues

### "No index found"

**Symptom**: All queries fail with "No index found for workspace"

**Solutions**:

1. **Build index**:
   ```bash
   cd /your/project
   sqry index .
   ```

2. **Verify index exists**:
   ```bash
   ls -la .sqry/graph
   # Should see: .sqry/graph and possibly .sqry/graph.lock
   ```

3. **Check workspace root**:
   ```json
   {
     "env": {
       "SQRY_MCP_WORKSPACE_ROOT": "/absolute/path/to/project"
     }
   }
   ```

4. **Test index directly**:
   ```bash
   sqry query "kind:function" /your/project --json
   # Should return functions
   ```

### Index Build Fails

**Symptom**: `sqry index .` fails with error

**Solutions**:

1. **Check permissions**:
   ```bash
   ls -la .
   # Should have write permission
   ```

2. **Check disk space**:
   ```bash
   df -h .
   # Need ~1-5% of codebase size
   ```

3. **Remove corrupted index**:
   ```bash
   rm -rf .sqry/graph
   sqry index .
   ```

4. **Check for file errors**:
   - Look for syntax errors in code files
   - Check logs for specific failures

### Stale Index

**Symptom**: Results don't reflect recent changes

**Solutions**:

1. **Rebuild index**:
   ```bash
   sqry index --force .
   ```

2. **Set up auto-rebuild** (if using frequently):
   - Use git hooks to rebuild on commit
   - Or rebuild daily via cron

3. **Check index age**:
   ```bash
   sqry index --status .
   # Shows when index was built
   ```

4. **Snapshot version mismatch**: The current snapshot format is V7. After upgrading sqry across major versions, you must rebuild the index:
   ```bash
   rm -rf .sqry/graph
   sqry index .
   ```

---

## Read-Only Graph Acquisition (Shared Contract)

Since the Shared Graph Acquisition feature (2026-05-08), CLI, standalone
`sqry-mcp`, daemon-hosted MCP, and `sqry-lsp` route read-only graph access
through a single shared contract
(`sqry_core::graph::acquisition::GraphAcquirer`). The behavior described
below applies to MCP clients connected via `sqry-mcp --daemon`.

### Reload-on-Evicted Behavior

When the daemon classifies a workspace as `Evicted` (LRU memory eviction),
the 14 read-only graph-backed daemon-hosted MCP tools attempt **one
bounded reload** from the existing persisted snapshot before failing.
For most evictions this is transparent — callers do not see
`WorkspaceEvicted` and the call succeeds.

**Tools covered by reload-on-evicted**:
`complexity_metrics`, `dependency_impact`, `direct_callees`, `direct_callers`,
`export_graph`, `find_cycles`, `find_unused`, `is_node_in_cycle`,
`relation_query`, `semantic_diff`, `semantic_search`, `show_dependencies`,
`subgraph`, `trace_path`.

**Mutating-tool exception**: `rebuild_index` does **NOT** use the
read-only reload fallback. Failures on the rebuild path are reported via
the explicit rebuild flow, not through reload.

### Remaining Error Cases

| Error | IPC Code | Cause | Recovery |
|-------|---------:|-------|----------|
| `WorkspaceEvicted` | -32004 | Reload after eviction failed: snapshot missing, corrupt, or no longer plugin-compatible. | `sqry index <root>` to rebuild a missing snapshot; `sqry index --force <root>` for corrupt or incompatible. |
| `WorkspaceIncompatibleGraph` | -32005 | Persisted snapshot uses an unknown plugin id or an incompatible snapshot format version. Distinct from eviction. | `sqry index --force <root>` to rebuild with the current plugin set / snapshot version. |
| `WorkspaceStaleExpired` | -32002 | Stale serve window expired. Distinct from eviction. | Trigger a rebuild (`sqry daemon rebuild <root>` or `rebuild_index` MCP tool) or wait for the next scheduled rebuild. |

### Recovery Actions

- **Missing snapshot**: `sqry index <root>` (build from scratch).
- **Corrupt or incompatible snapshot**: `sqry index --force <root>`.
- **Stuck workspace state**: `sqry daemon stop && sqry daemon start`.
- **Repeated evictions**: increase `memory_limit_mb` in `daemon.toml`, or
  `SQRY_DAEMON_MEMORY_MB`, so the working set fits the budget.

---

## Query Issues

### No Results

**Symptom**: Queries return empty results

**Solutions**:

1. **Test with broad query**:
   ```
   @sqry find all functions
   # Should return something
   ```

2. **Check index content**:
   ```bash
   sqry index --status .
   # Symbol count should be > 0
   ```

3. **Verify file is indexed**:
   ```bash
   sqry query "file:./path/to/file.ts" . --json
   ```

4. **Check language support**:
   - sqry supports: TS, JS, Python, Rust, Go, Java, C, C++, etc.
   - Unsupported files won't be indexed

5. **Rebuild index**:
   ```bash
   sqry index --force .
   ```

### Timeout Errors

**Symptom**: "Query timeout exceeded" or "Request timed out"

**Solutions**:

1. **Increase timeout**:
   ```json
   {
     "env": {
       "SQRY_MCP_TIMEOUT_MS": "60000"  // 60 seconds
     }
   }
   ```

2. **Check project size**:
   ```bash
   sqry index --status .
   # >100K symbols may need longer timeout
   ```

3. **Simplify query**:
   - Use fewer filters
   - Reduce max_depth in dependency queries
   - Limit results

4. **Check system load**:
   ```bash
   top
   # High CPU/memory usage can slow queries
   ```

### Incorrect Results

**Symptom**: Results don't match expectations

**Solutions**:

1. **Verify query syntax**:
   ```
   # Wrong: type:function
   # Right: kind:function
   ```

2. **Check for typos** (even though fuzzy search helps):
   ```
   # May need more specific pattern
   @sqry search for "authenticate" not "auth"
   ```

3. **Use structured query**:
   ```
   # Instead of fuzzy search
   @sqry find functions with exact name "authenticate"
   ```

4. **Rebuild index**:
   ```bash
   sqry index --force .
   ```

---

## Tool-Specific Issues

### semantic_search

**Issue**: Fuzzy search too broad

**Solution**: Use more specific pattern:
```
@sqry search for "UserAuthService" not "auth"
```

**Issue**: Missing results

**Solution**: Try broader pattern:
```
@sqry search for "auth" (might find AuthService, authenticate, etc.)
```

### relation_query

**Issue**: "Symbol not found"

**Solutions**:
1. Use qualified name: `auth::UserService::authenticate`
2. Try unqualified: `authenticate`
3. Verify symbol is indexed: `@sqry search for authenticate`

**Issue**: No callers found

**Solutions**:
1. Symbol might have 0 callers (is it called?)
2. Try increasing max_depth
3. Check if symbol is exported/public

### semantic_diff

**Issue**: "Invalid git ref"

**Solutions**:
1. Verify ref exists: `git rev-parse <ref>`
2. Use commit hash instead of branch name
3. Check spelling: `main` vs `master`

**Issue**: No changes detected

**Solutions**:
1. Verify refs are different: `git log base..target`
2. Check if changes are semantic (not just comments)
3. Rebuild index for both refs

**Issue**: "Worktree creation failed"

**Solutions**:
1. Ensure git repository is clean
2. No existing worktrees: `git worktree list`
3. Enough disk space for worktree

### dependency_impact

**Issue**: Empty impact results

**Solutions**:
1. Symbol might have no dependencies
2. Increase max_depth: Default is 3, try 5-10
3. Enable include_indirect: `true`

**Issue**: Too many results

**Solutions**:
1. Reduce max_depth
2. Disable include_indirect
3. Filter by specific file patterns

---

## Performance Issues

### Expensive Operations

Some MCP tools are significantly more expensive than others. If your AI assistant seems to hang or timeout, it may be using one of these:

| Tool | Risk | Why | Mitigation |
|------|------|-----|------------|
| `rebuild_index` | HIGH | Full graph rebuild | Only when index stale; uses 10min timeout (`SQRY_MCP_INDEX_TIMEOUT_MS`) |
| `semantic_diff` | HIGH | Creates 2 git worktrees + builds 2 indexes | Use `filters.change_types` and `filters.symbol_kinds` to narrow scope |
| `find_cycles` | HIGH | Known timeouts on large graphs (238K+ nodes) | Use `max_results`, scope to specific files |
| `complexity_metrics` | HIGH | Can hang on large graphs with cycles | Always provide `file_path` to scope |
| `find_duplicates` | MEDIUM | Pairwise comparison, quadratic scaling | Filter by `language`, `symbol_kind`, or `file_path` |
| `find_unused` | MEDIUM | Full graph reachability scan | Filter by `scope` and `language` |
| `call_hierarchy` depth>2 | MEDIUM | Exponential expansion | Keep `max_depth` <= 2 |
| `dependency_impact` depth>3 | MEDIUM | Exponential expansion | Keep `max_depth` <= 3 |
| `trace_path` | MEDIUM | Combinatorial path finding | Keep `max_hops` <= 5 |

**Best practice**: Always provide scope constraints (`file_path`, `symbol_name`, language filters) for analysis tools.

### Plugin Cost Tiering

Some language plugins are classified as `HighWallClock` and excluded from the default index for performance:
- `json` — JSON config files
- `servicenow-xml` — ServiceNow XML records

If symbols from these languages are missing, either:
- Rebuild with `SQRY_INCLUDE_HIGH_COST=1`
- Enable specific plugins: `sqry index --enable-plugin json`

### Slow Queries

**Symptom**: Queries take >5 seconds

**Solutions**:

1. **Reduce result limits**:
   ```
   @sqry search for functions (limit 20)
   ```

2. **Optimize query**:
   ```
   # Slow: all functions
   # Fast: functions in specific file
   ```

3. **Rebuild index** (might be fragmented):
   ```bash
   sqry index --force .
   ```

4. **Check system resources**:
   ```bash
   # Check if system is under load
   top
   ```

### High Memory Usage

**Symptom**: AI assistant using lots of RAM

**Solutions**:

1. **Reduce output size**:
   ```json
   {
     "env": {
       "SQRY_MCP_MAX_OUTPUT_BYTES": "25000"
     }
   }
   ```

2. **Limit result counts**:
   ```
   @sqry search for functions (limit 20)
   ```

3. **Rebuild index**:
   ```bash
   rm -rf .sqry/graph
   sqry index .
   ```

### Server Startup Slow

**Symptom**: AI assistant slow to start

**Solutions**:

1. **Check index size**:
   ```bash
   du -sh .sqry/graph
   # >100MB is unusual
   ```

2. **Use the rmcp server binary**:
   ```json
   {
     "command": "/path/to/target/release/sqry-mcp"
   }
   ```

3. **Profile startup**:
   - Check AI assistant logs for timing
   - Look for slow initialization

---

## Error Messages

### "Failed to spawn sqry process"

**Cause**: Can't execute sqry binary

**Solutions**:
1. Install sqry CLI: `cargo install --path sqry-cli`
2. Check permissions: `chmod +x $(which sqry)`

### "Path traversal attempt detected"

**Cause**: Query tried to access file outside workspace root

**Solutions**:
1. This is a security feature working correctly
2. Set SQRY_MCP_WORKSPACE_ROOT to project root
3. Use relative paths from project root

### "Output size exceeded limit"

**Cause**: Result too large for configured limit

**Solutions**:
1. Increase limit:
   ```json
   {
     "env": {
       "SQRY_MCP_MAX_OUTPUT_BYTES": "100000"
     }
   }
   ```
2. Reduce result count
3. Use more specific query

### "Manifest missing - run `sqry index`"

**Cause**: The MCP server requires a built index in `.sqry/graph/manifest.json` to operate. This file contains metadata about the indexed graph including version and content hashes.

**Solutions**:
1. Build the index:
   ```bash
   cd /your/project
   sqry index .
   ```

2. Verify manifest exists:
   ```bash
   ls -la .sqry/graph/manifest.json
   # Should exist and be readable
   ```

3. Check manifest is valid JSON:
   ```bash
   cat .sqry/graph/manifest.json | jq .
   # Should parse without errors
   ```

4. If manifest is corrupted, rebuild:
   ```bash
   rm -rf .sqry/graph
   sqry index .
   ```

**Why this changed**: Previous versions would silently fall back to less reliable freshness checks. Current version requires a valid manifest to ensure correct cache invalidation across multiple workspaces.

### "Manifest root_path mismatch"

**Cause**: The manifest's `root_path` field doesn't match the actual workspace directory. This prevents cross-workspace cache poisoning when `.sqry/graph/` directories are symlinked between repositories.

**Full error message**:
```
Manifest root_path mismatch: expected "/path/to/workspace-a", got "/path/to/workspace-b".
Possible symlinked .sqry/graph from different repo.
```

**Solutions**:
1. Rebuild the index in the correct workspace:
   ```bash
   cd /correct/workspace
   rm -rf .sqry/graph
   sqry index .
   ```

2. If you have symlinked `.sqry/graph/` (not recommended):
   ```bash
   # Remove symlink
   rm .sqry/graph

   # Build proper index
   sqry index .
   ```

3. Verify workspace paths:
   ```bash
   # Check where manifest thinks it belongs
   cat .sqry/graph/manifest.json | jq -r '.root_path'

   # Compare to actual workspace
   pwd
   ```

**Why this validation exists**: Sharing `.sqry/graph/` between repositories via symlinks can cause the MCP server to serve wrong-repository results. This validation protects against that scenario.

**Best practice**: Each repository should have its own `.sqry/graph/` directory. Don't symlink graph directories between projects.

### "Invalid JSON-RPC request"

**Cause**: Protocol error (usually internal)

**Solutions**:
1. Restart AI assistant
2. Check server version matches expected protocol
3. File GitHub issue with details

---

## Platform-Specific Issues

### macOS

**Issue**: "Cannot verify developer"

**Solution**:
```bash
xattr -d com.apple.quarantine /path/to/sqry-mcp
```

**Issue**: Config file not found

**Solution**: Create directory:
```bash
mkdir -p ~/Library/Application\ Support/Claude
```

### Linux

**Issue**: Permission denied

**Solution**:
```bash
chmod +x /path/to/sqry-mcp
chmod +x $(which sqry)
```

**Issue**: Config file location varies

**Solution**: Check XDG_CONFIG_HOME:
```bash
echo $XDG_CONFIG_HOME
# Config might be there instead of ~/.config
```

### Windows

**Issue**: Path with spaces

**Solution**: Use quotes in JSON:
```json
{
  "command": "C:\\Program Files\\sqry\\sqry-mcp.exe"
}
```

**Issue**: Backslashes in paths

**Solution**: Use forward slashes or double backslashes:
```json
{
  "command": "C:/Users/name/sqry/target/release/sqry-mcp.exe"
  // OR
  "command": "C:\\Users\\name\\sqry\\target\\release\\sqry-mcp.exe"
}
```

---

## AI Assistant Specific

### Claude Desktop

**Issue**: "Server failed to start"

**Check logs**:
```bash
tail -f ~/Library/Logs/Claude/mcp*.log
```

**Common causes**:
1. JSON syntax error in config
2. Binary not found
3. Binary not executable

**Issue**: Tools not appearing

**Solutions**:
1. Wait 10 seconds after startup
2. Start new chat (Cmd+N)
3. Restart Claude Desktop

### Windsurf

**Issue**: Server not loading

**Solutions**:
1. Open developer console (View → Toggle Developer Tools)
2. Check Console tab for errors
3. Reload window (Cmd/Ctrl+R)

**Issue**: "MCP server crashed"

**Solutions**:
1. Check MCP settings syntax
2. Verify binary path
3. Check Windsurf version

### Cursor

**Issue**: MCP not enabled

**Solution**: Enable in settings:
- Settings → Features → Enable MCP Protocol

**Issue**: Config file location wrong

**Solution**: Use `~/.cursor/mcp_settings.json`

### Codex CLI

**Issue**: sqry MCP not detected in Codex

**Solutions**:
1. Configure Codex integration: `sqry mcp setup --tool codex`
2. Verify status: `sqry mcp status`
3. Check config file: `~/.codex/config.toml`

**Issue**: Wrong workspace resolved

**Solutions**:
1. Start Codex from the intended project directory
2. Confirm index presence: `sqry index --status .`
3. Rebuild if needed: `sqry index --force .`

### Gemini CLI

**Issue**: sqry MCP not detected in Gemini

**Solutions**:
1. Configure Gemini integration: `sqry mcp setup --tool gemini`
2. Verify status: `sqry mcp status`
3. Check config file: `~/.gemini/settings.json`

**Issue**: Wrong workspace resolved

**Solutions**:
1. Start Gemini from the intended project directory
2. Confirm index presence: `sqry index --status .`
3. Rebuild if needed: `sqry index --force .`

---

## Debugging Tips

### Enable Debug Logging

**sqry CLI**:
```bash
RUST_LOG=debug sqry index .
```

**MCP Server** (if supported):
```json
{
  "env": {
    "RUST_LOG": "debug"
  }
}
```

### Test Tools Manually

**1. Test server is working**:
```bash
/path/to/sqry-mcp --list-tools | jq .
```

**2. Test specific tool**:
```bash
cat <<EOF | /path/to/sqry-mcp | jq .
{
  "jsonrpc": "2.0",
  "method": "tools/call",
  "params": {
    "name": "semantic_search",
    "arguments": {
      "pattern": "authenticate",
      "path": "/path/to/project"
    }
  },
  "id": 2
}
EOF
```

**3. Test with index**:
```bash
cd /your/project
sqry query "kind:function" . --json
```

### Check Protocol Communication

**Capture traffic** (advanced):
```bash
# Wrap server with logger
cat > /tmp/sqry-mcp-wrapper.sh <<'EOF'
#!/bin/bash
tee -a /tmp/mcp-input.log | /path/to/sqry-mcp | tee -a /tmp/mcp-output.log
EOF
chmod +x /tmp/sqry-mcp-wrapper.sh

# Use wrapper in config:
{
  "command": "/tmp/sqry-mcp-wrapper.sh"
}

# Check logs:
tail -f /tmp/mcp-input.log /tmp/mcp-output.log
```

---

## Getting More Help

### Collecting Diagnostics

When reporting issues, include:

1. **Environment**:
   ```bash
   # OS and version
   uname -a

   # sqry version
   sqry --version

   # Rust version (if building from source)
   rustc --version

   # AI assistant version
   # (from Help → About menu)
   ```

2. **Configuration**:
   ```bash
   # MCP config (remove sensitive paths)
   cat ~/Library/Application\ Support/Claude/claude_desktop_config.json
   ```

3. **Test results**:
   ```bash
   # Server test
   /path/to/sqry-mcp --list-tools

   # Index status
   sqry index --status /your/project
   ```

4. **Logs**:
   - AI assistant logs
   - MCP server logs
   - sqry CLI output

### Report an Issue

**File issue at**: https://github.com/verivus-oss/sqry/issues

**Include**:
- Environment info (above)
- Steps to reproduce
- Expected vs actual behavior
- Relevant logs
- Configuration (sanitized)

### Community Support

- **GitHub Discussions**: Ask questions
- **Documentation**: Check `docs/` for more guides
- **Examples**: See other users' workflows

---

## Known Limitations

### Current Limitations

1. **Index updates require rebuild or watch mode**
   - Use `sqry watch .` for automatic incremental updates on file changes (add `--build` on first run if no index exists)
   - Or manually rebuild: `sqry index --force .`

2. **Multi-workspace caching**
   - The server caches workspace engines keyed by path (capacity: `SQRY_MCP_ENGINE_CACHE_CAPACITY`)
   - Switching between many workspaces may evict cached engines, causing reload latency

3. **Git required for semantic_diff**
   - semantic_diff tool needs git repository
   - Other tools work without git

4. **Output size limits**
   - Large results truncated
   - Increase SQRY_MCP_MAX_OUTPUT_BYTES if needed

5. **Default redaction**
   - MCP responses are redacted by default (`SQRY_REDACTION_PRESET=minimal`)
   - If content appears missing from responses, set `SQRY_REDACTION_PRESET=none` to disable
   - Available presets: `none`, `minimal`, `standard`, `strict`

6. **Snapshot format version**
   - Current format is V7 (`SQRY_GRAPH_V7`)
   - Indexes from older sqry versions are not compatible; rebuild with `sqry index --force .`

---

## Best Practices to Avoid Issues

### Setup

1. **Use absolute paths** in config
2. **Set workspace root** for security
3. **Rebuild index** after major changes
4. **Test server** before configuring AI assistant

### Usage

1. **Start with simple queries** to test
2. **Use semantic_search** for exploration
3. **Use relation_query** for specific analysis
4. **Check index status** if getting no results

### Maintenance

1. **Rebuild index weekly** for active projects
2. **Update sqry CLI** when new versions release
3. **Monitor logs** for recurring errors
4. **Keep config backed up**

---

**Last Updated**: 2026-05-11
**MCP Server Version**: 15.0.1
**Protocol**: MCP 2024-11-05
