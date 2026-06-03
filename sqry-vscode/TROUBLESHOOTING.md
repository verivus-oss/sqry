# sqry VS Code Extension - Troubleshooting Guide

**Version**: 19.0.2
**Last Updated**: 2026-06-04

Quick solutions to common issues with the sqry VS Code extension.

---

## Quick Diagnostics

**Before troubleshooting**, run these checks:

1. **Check extension is activated**:
   - Open Command Palette (`Ctrl/Cmd+Shift+P`)
   - Type "Sqry" - you should see the sqry commands such as Search Workspace,
     Run Query, Index Workspace, Refresh Index Stats, and Restart Language Server

2. **Check sqry CLI**:
   ```bash
   sqry --version
   # Should output: sqry 19.0.2 or later
   ```

3. **Check extension logs**:
   - VS Code → View → Output
   - Select "sqry" from dropdown
   - Look for errors

4. **Check workspace**:
   ```bash
   cd /your/project
   ls -la .sqry-index  # Should exist if indexed
   ```

---

## Installation Issues

### Extension Not Installing

**Symptom**: Installation fails with error

**Common Causes**:
- Incompatible VS Code version
- Corrupted download
- Permission issues

**Solutions**:

1. **Check VS Code version**:
   ```
   Help → About
   # Need 1.85.0 or later
   ```

2. **Try manual installation**:
   ```bash
   # Download VSIX
   # Then:
   code --install-extension sqry-vscode-<version>.vsix --force
   ```

3. **Check permissions**:
   ```bash
   # Linux/Mac: Ensure ~/.vscode/extensions is writable
   ls -la ~/.vscode/extensions

   # Windows: Check %USERPROFILE%\.vscode\extensions
   ```

4. **Clear extension cache**:
   ```bash
   # Linux/Mac
   rm -rf ~/.vscode/extensions/verivus.sqry-vscode-*

   # Windows
   rmdir /s %USERPROFILE%\.vscode\extensions\verivus.sqry-vscode-*
   ```

### Extension Not Activating

**Symptom**: Extension installed but commands don't appear

**Solutions**:

1. **Reload window**:
   ```
   Command Palette → Reload Window
   ```

2. **Check extension is enabled**:
   - Extensions view (`Ctrl/Cmd+Shift+X`)
   - Search "sqry"
   - Ensure not disabled

3. **Check for conflicts**:
   - Temporarily disable other extensions
   - Reload window
   - Re-enable one by one to find conflict

4. **Check VS Code logs**:
   ```
   Help → Toggle Developer Tools → Console
   # Look for extension errors
   ```

---

## Binary Issues

### "sqry not found" Error

**Symptom**: Extension shows "Unable to locate sqry binary"

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

3. **Set explicit path in settings**:
   ```json
   {
     "sqry.path": "/home/user/.cargo/bin/sqry"  // Use full path
   }
   ```

4. **Check PATH**:
   ```bash
   echo $PATH      # Linux/Mac
   echo %PATH%     # Windows

   # Should include ~/.cargo/bin or similar
   ```

5. **Restart VS Code** after installing sqry

### "sqry version too old" Error

**Symptom**: Extension requires newer sqry version

**Solution**: Update sqry:
```bash
cd /path/to/sqry/repo
cargo install --path sqry-cli --force
sqry --version  # Verify: 19.0.2+
```

### Binary Execution Fails

**Symptom**: sqry binary exists but fails to execute

**Solutions**:

1. **Check permissions**:
   ```bash
   ls -l $(which sqry)
   # Should be executable: -rwxr-xr-x

   # Fix if needed:
   chmod +x $(which sqry)
   ```

2. **Test binary directly**:
   ```bash
   sqry index --status .
   # Should show index info or "No index found"
   ```

3. **Check for missing libraries** (Linux):
   ```bash
   ldd $(which sqry)
   # Look for "not found"
   ```

### "Failed to download sqry binary: Binary provenance could not..." Error

**Symptom**: The extension prompts to install sqry, then binary download fails
with a provenance or certificate-identity error.

**What it means**: The extension verifies release assets with checksum and
Sigstore/Cosign provenance. Current releases are expected to be signed by the
public `release-distribute.yml` workflow; older assets may have been signed by
the legacy `oss-distribute.yml` workflow.

**Solutions**:

1. **Update the extension** to 10.0.1 or later so both current and legacy
   workflow identities are accepted.
2. **Reload VS Code** after updating:
   ```
   Command Palette → Developer: Reload Window
   ```
3. **Install manually if your network blocks Sigstore/GitHub downloads**:
   - Download the matching release asset from GitHub releases.
   - Verify `SHA256SUMS.txt`.
   - Set `"sqry.path"` to the installed binary.
4. **Check the sqry output channel** for the exact workflow identity that was
   attempted.

---

## Index Issues

### "No index found"

**Symptom**: Commands fail with "No index found for workspace"

**Solution**: Build index first:

```
Command Palette → Sqry: Index Workspace
```

Or via CLI:
```bash
cd /your/project
sqry index .
```

### Multi-Root Workspace Still Shows "Not Indexed"

**Symptom**: You open a saved `.code-workspace`, indexing appears to run for
each folder, but the sqry pane still says the workspace is not indexed.

**What to check**:

1. **Verify you are on extension 10.0.1 or later**. Earlier builds could read a
   no-path `sqry/indexStatus` response as though it were the aggregate
   workspace status.
2. **Inspect the aggregate workspace status from the CLI**:
   ```bash
   sqry workspace status /path/to/workspace-root --json --no-cache
   ```
   Source roots should show `ok` after indexing. `missing` means that source
   root still needs an index.
3. **Check your `.code-workspace` classification**. Source roots should be
   listed under `sqry.workspace.sourceRoots`; docs-only folders should be
   `memberFolders`.
   ```jsonc
   {
     "sqry.workspace": {
       "sourceRoots": ["services/auth", "services/billing"],
       "memberFolders": ["docs"],
       "exclusions": ["vendor"],
       "projectRootMode": "gitRoot"
     }
   }
   ```
4. **Reload VS Code** after changing the workspace file:
   ```
   Command Palette → Developer: Reload Window
   ```

### Indexing Timeouts

**Symptom**: "Indexing timed out after X seconds"

**Solutions**:

1. **Increase timeout** (for large projects):
   ```json
   {
     "sqry.indexTimeoutMs": 600000  // 10 minutes
   }
   ```

2. **Index via CLI** (faster):
   ```bash
   sqry index --force .
   ```

3. **Check project size**:
   ```bash
   sqry index --status .
   # Shows: files, symbols, size
   ```
   - >100K symbols: Use CLI for initial index
   - >10K files: Increase timeout to 10-20 minutes

4. **Exclude large directories**:
   - Add to `.gitignore`: `node_modules/`, `build/`, `dist/`
   - sqry respects `.gitignore`

### Index Build Fails

**Symptom**: Indexing starts but fails with error

**Solutions**:

1. **Check disk space**:
   ```bash
   df -h .
   # Index needs ~1-5% of codebase size
   ```

2. **Check permissions**:
   ```bash
   ls -la .sqry-index
   # Should be readable/writable

   # Fix if needed:
   rm -rf .sqry-index
   sqry index .
   ```

3. **Check for corrupted files**:
   - Look for syntax errors in code
   - Check logs for specific file failures

4. **Try force rebuild**:
   ```bash
   sqry index --force .
   ```

### "Index is stale" Warning

**Symptom**: Warning that index is outdated (>24 hours old)

**Solution**: Rebuild index:
```
Command Palette → Sqry: Index Workspace
```

Or CLI:
```bash
sqry index --force .
```

**Prevent**: Set up auto-indexing:
```json
{
  "sqry.autoIndexOnOpen": "always"
}
```

### Missing Results After Index

**Symptom**: Index built successfully but searches return no results

**Solutions**:

1. **Verify index content**:
   ```bash
   sqry index --status .
   # Check symbol count is > 0
   ```

2. **Check file language support**:
   - sqry supports: TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, etc.
   - Unsupported files won't be indexed

3. **Test with simple query**:
   ```
   Command Palette → Sqry: Query... → "kind:function"
   # Should return all functions
   ```

4. **Rebuild index**:
   ```bash
   rm -rf .sqry-index
   sqry index .
   ```

---

## Search & Query Issues

### No Results for Known Symbol

**Symptom**: Searching for symbol you know exists returns nothing

**Solutions**:

1. **Try fuzzy search**:
   ```
   Sqry: Search Workspace... → "authnticate"  # Typo-tolerant
   ```

2. **Use regex**:
   ```
   Sqry: Query... → "name~=/auth/"  # Contains "auth"
   ```

3. **Broaden search**:
   ```
   # Instead of: name:authenticate
   # Try: name~=/auth/
   ```

4. **Check if indexed**:
   ```bash
   sqry query "kind:function" . | grep authenticate
   ```

5. **Rebuild index** (might be stale or corrupted)

### Query Syntax Errors

**Symptom**: Query fails with "Invalid query syntax"

**Common Mistakes**:

1. **Wrong operator**:
   ```
   # Wrong: name=authenticate
   # Right: name:authenticate
   ```

2. **Unescaped regex**:
   ```
   # Wrong: name~=/user.service/
   # Right: name~=/user\.service/
   ```

3. **Missing quotes**:
   ```
   # Wrong: name:my function
   # Right: name:"my function"
   ```

4. **Wrong field name**:
   ```
   # Wrong: type:function
   # Right: kind:function
   ```

**Valid Fields**:
- `kind:` - Symbol kind (function, class, method, etc.)
- `name:` - Symbol name (exact match)
- `name~=/regex/` - Symbol name (regex match)
- `file:` - File path
- `async:` - true/false
- `visibility:` - public/private/protected
- `returns:` - Return type

### Too Many Results

**Symptom**: Query returns thousands of results

**Solutions**:

1. **Add more filters**:
   ```
   # Instead of: kind:function
   # Try: kind:function AND file:./src/ AND async:true
   ```

2. **Reduce limit**:
   ```json
   {
     "sqry.limit": 50  // Default: 200
   }
   ```

3. **Use pagination**:
   - Results panel shows first N results
   - Scroll to load more

4. **Use more specific query**:
   ```
   # Instead of: name~=/./
   # Try: name~=/^handle/ AND kind:function
   ```

### Slow Queries

**Symptom**: Queries take >5 seconds to complete

**Solutions**:

1. **Reduce result limit**:
   ```json
   {
     "sqry.limit": 50
   }
   ```

2. **Use more specific queries**:
   ```
   # Slow: name~=/./
   # Fast: kind:function AND file:./src/auth.ts
   ```

3. **Increase timeout** (if queries timeout):
   ```json
   {
     "sqry.timeoutMs": 30000  // 30 seconds
   }
   ```

4. **Rebuild index** (might be fragmented):
   ```bash
   sqry index --force .
   ```

---

## UI Issues

### Results Panel Not Showing

**Symptom**: Run query but results panel doesn't appear

**Solutions**:

1. **Open panel manually**:
   ```
   View → Open View... → "Semantic Results"
   ```

2. **Check panel is visible**:
   - Look in Explorer sidebar
   - May be minimized or collapsed

3. **Reset panel**:
   ```
   Right-click on panel → Reset View Location
   ```

4. **Reload window**:
   ```
   Command Palette → Reload Window
   ```

### Progress Indicators Not Showing

**Symptom**: "sqry: Index Workspace" runs but no progress notification appears

**Solutions**:

1. **Check sqry version**:
   ```bash
   sqry --version
   # Extension version 19.0.2+ required for progress indicators
   ```

2. **Check notifications are enabled**:
   - VSCode Settings → Notifications
   - Ensure "Do Not Disturb" mode is disabled

3. **Check extension is connected**:
   - View → Output → sqry
   - Look for "sqry-lsp ready" message

4. **Enable LSP logging**:
   ```json
   {
     "sqry.trace.server": "verbose"
   }
   ```
   Then check Output panel for `$/progress` entries.

5. **Very fast indexing**:
   - For small projects (<100 files), indexing may complete in <1 second
   - Progress notifications are throttled to 200ms intervals
   - This is expected behavior - you'll only see the completion message

6. **Try manual index**:
   ```bash
   sqry index .
   # CLI shows detailed progress in terminal
   ```

7. **Restart extension**:
   ```
   Command Palette → sqry: Restart Language Server
   ```

**If progress still doesn't show**, file an issue with:
- sqry --version output
- Extension version
- VSCode version
- Output panel logs (Output → sqry)

### CodeLens Not Appearing

**Symptom**: No caller counts shown above functions

**Solutions**:

1. **Check if enabled**:
   ```json
   {
     "sqry.codeLens.enabled": true
   }
   ```

2. **Verify index exists**:
   ```bash
   ls -la .sqry-index  # Must exist
   ```

3. **Wait for indexing**:
   - CodeLens appears after index completes
   - Check status bar for progress

4. **Reload window**:
   ```
   Command Palette → Reload Window
   ```

5. **Check file type**:
   - CodeLens only appears in supported languages
   - TypeScript, JavaScript, Python, Rust, etc.

### Results Panel Empty

**Symptom**: Results panel opens but shows "No results"

**Solutions**:

1. **Check query succeeded**:
   - Look for errors in Output → sqry

2. **Try broader query**:
   ```
   kind:function  # Should return all functions
   ```

3. **Verify index has symbols**:
   ```bash
   sqry index --status .
   # Should show symbol count > 0
   ```

4. **Rebuild index**:
   ```bash
   sqry index --force .
   ```

---

## Performance Issues

### High Memory Usage

**Symptom**: VS Code using lots of RAM after using sqry

**Solutions**:

1. **Close results panel** when not needed:
   ```
   Click X on Semantic Results panel
   ```

2. **Reduce result limit**:
   ```json
   {
     "sqry.limit": 50  // Default: 200
   }
   ```

3. **Clear results**:
   - Click 🗑️ (clear) button in results panel

4. **Restart VS Code**:
   ```
   Command Palette → Reload Window
   ```

### Extension Slow to Load

**Symptom**: VS Code takes long to start with sqry extension

**Solutions**:

1. **Disable auto-indexing**:
   ```json
   {
     "sqry.autoIndexOnOpen": "never"
   }
   ```

2. **Check index size**:
   ```bash
   du -sh .sqry-index
   # >100 MB is unusual, consider rebuilding
   ```

3. **Profile extension**:
   ```
   Command Palette → Developer: Show Running Extensions
   # Check sqry activation time
   ```

### Slow CodeLens Updates

**Symptom**: CodeLens takes long to appear or update

**Solutions**:

1. **Disable CodeLens in large files**:
   - Files >5000 lines may be slow
   - Disable per-file or globally

2. **Check index status**:
   ```bash
   sqry index --status .
   ```

3. **Rebuild index**:
   ```bash
   sqry index --force .
   ```

---

## Integration Issues

### Conflicts with Other Extensions

**Symptom**: sqry or other extensions stop working

**Common Conflicts**:
- Other semantic search extensions
- Multiple LSP servers for same language
- Extensions that modify file system watch

**Solutions**:

1. **Identify conflict**:
   - Disable all other extensions
   - Enable one by one
   - Note which causes conflict

2. **Disable conflicting extension**:
   - Or use workspace-specific settings

3. **Report conflict**:
   - File GitHub issue with details

### LSP Server Not Starting

**Symptom**: Hover, go-to-definition don't work

**Note**: sqry extension uses sqry CLI, not LSP. For LSP features:
- Use `sqry lsp --stdio` for LSP integration
- See `sqry-lsp/` for the LSP server implementation

### Multi-Root Workspace Issues

**Symptom**: Extension doesn't work in multi-root workspace

**Solutions**:

1. **Index each root separately**:
   ```bash
   cd /workspace/root1
   sqry index .

   cd /workspace/root2
   sqry index .
   ```

2. **Use workspace-relative paths** in queries:
   ```
   file:./root1/src/auth.ts
   ```

3. **Check current workspace**:
   - Extension uses workspace where active file is located

---

## Platform-Specific Issues

### Linux

**Issue**: Permission denied on index file

**Solution**:
```bash
chmod -R u+rw .sqry-index
```

**Issue**: sqry binary not in PATH

**Solution**:
```bash
# Add to ~/.bashrc or ~/.zshrc
export PATH="$HOME/.cargo/bin:$PATH"
```

### macOS

**Issue**: "sqry can't be opened because Apple cannot check it"

**Solution**:
```bash
xattr -d com.apple.quarantine $(which sqry)
```

**Issue**: Extension slow on network drives

**Solution**:
- Index on local disk
- Or increase timeouts significantly

### Windows

**Issue**: Path with spaces causes errors

**Solution**: Use quotes in settings:
```json
{
  "sqry.path": "C:\\Program Files\\sqry\\sqry.exe"
}
```

**Issue**: Index file locked

**Solution**:
```powershell
# Close all VS Code windows
# Delete index and rebuild
Remove-Item -Recurse -Force .sqry-index
sqry index .
```

---

## Error Messages

### "Failed to execute sqry binary"

**Possible Causes**:
- Binary not found
- Binary not executable
- Missing dependencies

**Solutions**:
1. Check `sqry.path` setting
2. Verify binary exists: `ls -l $(which sqry)`
3. Test binary: `sqry --version`
4. Check permissions: `chmod +x $(which sqry)`

### "Index build failed: permission denied"

**Cause**: No write permission to project directory

**Solutions**:
1. Check permissions: `ls -la .`
2. Fix if needed: `chmod u+w .`
3. Try different directory

### "Query timeout exceeded"

**Cause**: Query took too long

**Solutions**:
1. Increase timeout:
   ```json
   {
     "sqry.timeoutMs": 30000  // 30 seconds
   }
   ```
2. Simplify query
3. Reduce result limit

---

## Getting More Help

### Logs and Diagnostics

**Extension logs**:
```
VS Code → Output → sqry (dropdown)
```

**CLI logs**:
```bash
RUST_LOG=debug sqry index .
```

**VS Code logs**:
```
Help → Toggle Developer Tools → Console
```

### Report an Issue

When reporting issues, include:

1. **Environment**:
   - OS and version
   - VS Code version
   - sqry version (`sqry --version`)
   - Extension version

2. **Reproduction steps**:
   - Exact commands/actions
   - Expected vs actual behavior

3. **Logs**:
   - Extension output (Output → sqry)
   - VS Code console errors
   - CLI output if relevant

4. **Project info** (if relevant):
   - Project size (files, symbols)
   - Languages used
   - Index status

**File issue at**: https://github.com/verivus-oss/sqry/issues

### Community Support

- **GitHub Discussions**: Ask questions, share tips
- **Documentation**: Check `docs/` for more guides
- **Examples**: See `QUICKSTART.md` for query examples

---

## Known Limitations

### Current Limitations

1. **Limited UI customization** - Basic panel styling
2. **No workspace sync** - Index per workspace

### Feature Requests

Suggest new features via GitHub Discussions or Issues!
