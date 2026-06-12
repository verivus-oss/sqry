# sqry VS Code Extension - User Guide

**Version**: 20.0.3
**Last Updated**: 2026-06-13

---

## Table of Contents

1. [Overview](#overview)
2. [Installation](#installation)
3. [Getting Started](#getting-started)
4. [Features](#features)
5. [Configuration](#configuration)
6. [Common Workflows](#common-workflows)
7. [Query Syntax](#query-syntax)
8. [Tips & Best Practices](#tips--best-practices)
9. [Troubleshooting](#troubleshooting)

---

## Overview

The sqry VS Code extension brings **semantic code search** directly into your editor, enabling you to:

- **Search** for code symbols using fuzzy matching (typo-tolerant)
- **Query** your codebase with structured queries (boolean logic, filters)
- **Navigate** semantic relationships (callers, callees, references)
- **Analyze** code structure with inline insights (CodeLens)
- **Explore** results in a dedicated panel with symbol and text matches
- **Inspect** index health, duplicates, cycles, and unused symbols
- **Review** cross-language relationships in a dedicated panel

### What Makes sqry Different?

Unlike text-based search (Ctrl+Shift+F) or symbol search (Ctrl+T), sqry understands:
- **Semantic relationships**: Find who calls a function, what imports a module
- **Cross-language connections**: Track imports across TypeScript, Python, Rust, etc.
- **Code structure**: Filter by async/sync, visibility, return types
- **Fuzzy matching**: Typo-tolerant search that still finds what you need

---

## Installation

### Prerequisites

**Required**:
1. **VS Code** 1.85.0 or later
2. **sqry CLI** installed and accessible in PATH

**Install sqry CLI**:
```bash
# From source (requires Rust)
cargo install --path sqry-cli

# Verify installation
sqry --version
# Should output: sqry 20.0.3 (or later)
```

### Option 1: Install from VSIX

1. **Download** the latest VSIX file:
   - From releases: `sqry-vscode-<version>.vsix`
   - Or build locally (see below)

2. **Install** in VS Code:
   ```bash
   # Via command line
   code --install-extension sqry-vscode-<version>.vsix

   # Or via VS Code UI:
   # 1. Open Command Palette (Ctrl/Cmd+Shift+P)
   # 2. Type "Extensions: Install from VSIX..."
   # 3. Select the downloaded .vsix file
   ```

3. **Reload** VS Code when prompted

### Option 2: Install from Source (Developers)

```bash
# Clone repository
git clone https://github.com/verivus-oss/sqry.git
cd sqry/sqry-vscode

# Install dependencies
npm install

# Compile extension
npm run compile

# Package (optional)
npx @vscode/vsce package
# Creates: sqry-vscode-<version>.vsix

# Install
code --install-extension sqry-vscode-<version>.vsix
```

### Option 3: Run from Source (Development Mode)

```bash
cd sqry/sqry-vscode
npm install
npm run compile

# Open in VS Code
code .

# Press F5 to launch Extension Development Host
```

### Auto-Download Release Contract

The extension's auto-download path is intentionally strict in normal
production use:

- it downloads only from `https://github.com/verivus-oss/sqry/releases`
- it verifies the published checksum
- it verifies the Sigstore/Cosign attestation bundle and workflow identity
  (`release-distribute.yml` for current releases, with `oss-distribute.yml`
  retained for legacy release compatibility)

For marketplace/Open VSX installs, the requested `binaryVersion` must already
exist as a public GitHub release. If the exact release has not been published
yet, auto-download will fail closed rather than silently pulling an arbitrary
binary.

Public auto-download binaries are currently expected for:

- Linux `x86_64`
- Linux `arm64`
- Windows `x86_64`
- macOS `arm64`
- macOS Intel (`x86_64`)

For Extension Development Host / test runs launched from source, the downloader
may fall back to the latest published patch release in the same `major.minor`
line when the exact requested patch version is not yet public. This is only a
development convenience so local extension testing can continue while the public
release is still catching up.

For the canonical CLI workspace workflow, see
[docs/user-guide/workspace.md](../docs/user-guide/workspace.md). For MCP setup
and daemon-backed assistant workflows, see
[docs/user-guide/mcp.md](../docs/user-guide/mcp.md) and
[docs/user-guide/daemon.md](../docs/user-guide/daemon.md).

---

## Getting Started

### 1. Index Your First Project

Before searching, you need to **build an index** of your codebase:

**Option A: Via Extension (Recommended)**

1. Open your project in VS Code
2. Open Command Palette (`Ctrl/Cmd+Shift+P`)
3. Type: `Sqry: Index Workspace`
4. Wait for indexing to complete (progress shown in status bar)

**Option B: Via CLI**

```bash
cd /path/to/your/project
sqry index .
```

**What gets indexed?**
- All code files in supported languages (TypeScript, Python, Rust, Go, Java, etc.)
- Symbols: functions, classes, methods, variables, imports, exports
- Relationships: callers, callees, imports, references
- sqry skips common dependency/generated/cache roots by default: `.git`, `.hg`,
  `.svn`, `.cache`, `.next`, `.nuxt`, `.sqry`, `.turbo`, `.venv`,
  `__pycache__`, `_actions`, `_update`, `_work`, `build`, `dist`,
  `node_modules`, `target`, `vendor`, `venv`, and `externals.*`. Set
  `SQRY_INCLUDE_DEFAULT_EXCLUDED_DIRS=1` before launching VS Code if your
  workspace intentionally keeps first-party code in one of those directories.

**Index location**: `.sqry-index` in your project root (gitignore this!)

### 2. Run Your First Search

**Simple Text Search**:

1. Open Command Palette (`Ctrl/Cmd+Shift+P`)
2. Type: `Sqry: Search Workspace...`
3. Enter: `authenticate`
4. View results in "Semantic Results" panel (sidebar)

**Structured Query**:

1. Open Command Palette
2. Type: `Sqry: Query...`
3. Enter: `kind:function AND async:true`
4. See all async functions in your codebase!

### 3. Navigate Code Relationships

**Find Callers**:

1. Place cursor on a function name
2. Right-click → `Find Callers` (or Command Palette: `Sqry: Find Callers`)
3. See all functions that call this one

**View CodeLens**:

After indexing, you'll see **caller counts** above functions:
```typescript
// 5 callers | Find Callers | Show References
function authenticate(user: string, password: string) { ... }
```

Click on the CodeLens to:
- View all callers
- Show all references
- Get detailed explanation

---

## Features

### 1. Command Palette Actions

Access all features via Command Palette (`Ctrl/Cmd+Shift+P`):

| Command | Description | Shortcut |
|---------|-------------|----------|
| `Sqry: Query...` | Run structured query with filters | - |
| `Sqry: Search Workspace...` | Fuzzy search for symbols | - |
| `Sqry: Find Semantic References` | Find all references to symbol at cursor | - |
| `Sqry: Find Callers` | Find who calls this function | - |
| `Sqry: Explain Symbol` | Get detailed explanation of symbol | - |
| `Sqry: Index Workspace` | Build/rebuild semantic index | - |
| `Sqry: Refresh Index Stats` | Refresh index status in the results panel | - |
| `Sqry: Clear Results` | Clear the results panel | - |

### 2. Semantic Results Panel

After running a query, results appear in the **"Semantic Results"** panel (sidebar):

**Symbol Results**:
- Grouped by file
- Shows symbol name, kind (function/class/etc.), line number
- Click to jump to definition

**Text Matches**:
- Line-by-line text matches
- Shows context around match
- Click to open file at that line

**Panel Features**:
- Refresh results (🔄)
- Clear results (✕)
- Expand/collapse all (▼/▶)

**Analysis & Diagnostics**:
- **Index Status**: symbol/file counts, cross-language edge counts, build age, and health (stats auto-refresh after rebuild)
- **Duplicates**: grouped duplicate symbols (body/signature/struct) — shows "expand to check" until first load
- **Circular Dependencies**: call/import cycles — shows "expand to check" until first load
- **Unused Symbols**: reachability-based dead code — shows "expand to check" until first load; displays truncation indicator when results are limited
- **Cross-Language Relations**: imports/calls crossing language boundaries with per-language-pair counts

### 3. CodeLens Integration

**After indexing**, you'll see inline annotations above functions:

```typescript
// 5 callers | Find Callers | Show References
export function processUser(id: string) { ... }
```

**What you see**:
- **Caller count**: How many functions call this
- **Quick actions**: Click to find callers or references

**Disable CodeLens**:
```json
{
  "sqry.codeLens.enabled": false
}
```

### 4. Code Actions (Right-Click Menu)

Right-click on any symbol to access:

- **Find Callers**: Who calls this function?
- **Show References**: All places this symbol is used
- **Explain Symbol**: Detailed info about this symbol

### 5. Hover Information

Hover over indexed symbols to see:
- Symbol kind (function, class, method, etc.)
- Visibility (public, private, etc.)
- Type information
- Caller count

### 6. Index Stats Auto-Refresh And Workspace Status

After rebuilding the index (via Command Palette or auto-index), the extension automatically refreshes the index status panel with updated statistics including:
- Symbol and file counts
- Language breakdown
- Cross-language edge counts (per language pair)
- Index age and health

For saved multi-root `.code-workspace` files, the panel reads the LSP
`sqry/workspaceStatus` aggregate rather than treating a no-path
`sqry/indexStatus` response as the whole workspace. Each configured source root
can therefore show `ok`, `building`, `missing`, or `error` independently, and a
healthy multi-root workspace does not collapse into a false "not indexed" state.

This means the Semantic Results panel reflects the latest authoritative
workspace state without needing to manually refresh.

### 7. Multi-Root Workspace Classification

Cross-repo analysis is opt-in. Add a `sqry.workspace` block to a saved
`.code-workspace` file to tell sqry which folders are source roots, which are
member folders, and which paths should never be indexed:

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

The extension forwards the workspace-file path to `sqry lsp`, and the LSP loads
the `.code-workspace` file directly as the authoritative logical workspace.
Use `Sqry: Edit Workspace Classification (.code-workspace)` to seed or edit
this block from VS Code.

### 8. Auto-Indexing

**On workspace open**, the extension can:
- **Prompt** you to index (default)
- **Always** index automatically
- **Never** index (manual only)

Index checks performed before prompting:
- Detects `.sqry-index.lock` to avoid duplicate builds
- Validates index health via LSP `sqry/indexStatus` when available
- Falls back to index age (stale if older than 7 days)

Configure via:
```json
{
  "sqry.autoIndexOnOpen": "prompt"  // "always", "prompt", or "never"
}
```

---

## Configuration

### Settings Overview

Open VS Code Settings (`Ctrl/Cmd+,`) and search for "sqry":

| Setting | Default | Description |
|---------|---------|-------------|
| `sqry.path` | `"sqry"` | Path to sqry binary |
| `sqry.limit` | `200` | Max results per query |
| `sqry.timeoutMs` | `15000` | Query timeout (15s) |
| `sqry.indexTimeoutMs` | `300000` | Index timeout (5 min) |
| `sqry.autoIndexOnOpen` | `"prompt"` | Auto-index behavior |
| `sqry.autoIndexOnSave` | `"never"` | Optional debounced rebuild after file saves |
| `sqry.indexRoot` | `""` | Optional LSP index-root override |
| `sqry.projectRootMode` | `"gitRoot"` | Extension-side project root detection mode |
| `sqry.workspaceFolderExcludes` | `[]` | Workspace folders skipped by extension enumeration loops |
| `sqry.workspaceClassification` | `null` | User-editable classification used to write a `.code-workspace` `sqry.workspace` block |
| `sqry.codeLens.enabled` | `true` | Show CodeLens annotations |

### Configuration Examples

**Workspace Settings** (`.vscode/settings.json`):

```json
{
  // Custom sqry binary location
  "sqry.path": "/usr/local/bin/sqry",

  // More results per query
  "sqry.limit": 500,

  // Faster queries for small projects
  "sqry.timeoutMs": 5000,

  // Longer timeout for large codebases (10,000+ files)
  "sqry.indexTimeoutMs": 600000,  // 10 minutes

  // Always auto-index
  "sqry.autoIndexOnOpen": "always",

  // Disable CodeLens
  "sqry.codeLens.enabled": false
}
```

**User Settings** (global):

```json
{
  "sqry.path": "sqry",
  "sqry.limit": 200,
  "sqry.codeLens.enabled": true
}
```

### Timeout Configuration

**Two separate timeouts**:

1. **`sqry.timeoutMs`** (default: 15s) - Quick operations:
   - Searches
   - Queries
   - Finding callers/references
   - Symbol lookups

2. **`sqry.indexTimeoutMs`** (default: 5 minutes) - Index builds:
   - Initial workspace indexing
   - Rebuilding stale index
   - Large codebase processing

**When to increase timeouts**:
- **Large codebases** (10,000+ symbols): Increase `indexTimeoutMs` to 10-20 minutes
- **Slow network drives**: Increase both timeouts by 2-3x
- **Complex queries**: Increase `timeoutMs` to 30-60 seconds

---

## Common Workflows

### Workflow 1: Understanding a New Codebase

**Goal**: Quickly understand how authentication works

1. **Index** the project:
   ```
   Command Palette → Sqry: Index Workspace
   ```

2. **Search** for authentication:
   ```
   Command Palette → Sqry: Search Workspace... → "authenticate"
   ```

3. **Find callers** of main auth function:
   ```
   Click on function → Right-click → Find Callers
   ```

4. **Explore relationships**:
   - See who calls the auth function
   - Trace back to entry points (controllers, routes)
   - Understand the auth flow

### Workflow 2: Refactoring a Function

**Goal**: Safely rename/modify a function

1. **Find all callers**:
   ```
   Caret on function → Right-click → Find Callers
   ```

2. **Check references**:
   ```
   Right-click → Show References
   ```

3. **Review impact**:
   - See all places that call this function
   - Identify test files that need updates
   - Find documentation that mentions it

4. **Make changes** confidently knowing the full impact

### Workflow 3: Finding Examples

**Goal**: Find examples of async error handling

**Query**:
```
Command Palette → Sqry: Query... → "kind:function AND async:true AND name~=/error|catch/"
```

**Results**: All async functions with "error" or "catch" in the name

**Refine**:
```
kind:function AND async:true AND name~=/handle.*error/
```

### Workflow 4: Debugging a Bug

**Goal**: Find where a variable is set

1. **Search** for the variable:
   ```
   Sqry: Search Workspace... → "userPermissions"
   ```

2. **Filter results** by file:
   - Click through results in different files
   - Look for assignments vs. reads

3. **Find who calls** the setter:
   ```
   Right-click on setter → Find Callers
   ```

4. **Trace** the data flow back to the source

### Workflow 5: Code Review

**Goal**: Review changes to a module

1. **Find all exports** from the module:
   ```
   Sqry: Query... → "file:./src/auth.ts AND kind:export"
   ```

2. **Check who imports** each export:
   ```
   Right-click on export → Show References
   ```

3. **Verify** no unexpected usage

---

## Query Syntax

### Basic Queries

**Kind-based**:
```
kind:function        # All functions
kind:class          # All classes
kind:method         # All methods
```

**Name-based**:
```
name:authenticate   # Exact name match
name~=/auth/       # Regex: contains "auth"
name~=/^test/      # Regex: starts with "test"
```

**Attribute-based**:
```
async:true         # Async functions
visibility:public  # Public symbols
static:true        # Static methods
```

### Boolean Logic

**AND**:
```
kind:function AND async:true
kind:method AND visibility:private
```

**OR**:
```
kind:function OR kind:method
name~=/test/ OR name~=/spec/
```

**NOT**:
```
kind:function NOT name~=/private/
async:true NOT visibility:private
```

**Grouping**:
```
(kind:function OR kind:method) AND async:true
kind:class AND (visibility:public OR visibility:protected)
```

### Advanced Filters

**File-based**:
```
file:./src/auth.ts                # Specific file
file~=/src\/.*\.ts$/             # TypeScript files in src/
```

**Return type**:
```
returns:Promise                   # Returns Promise
returns~=/Result<.*>/            # Returns Result type
```

**Language**:
```
lang:typescript AND kind:interface
lang:python AND kind:class
```

### Example Queries

**Find all public async functions**:
```
kind:function AND async:true AND visibility:public
```

**Find test files**:
```
file~=/test|spec/ OR name~=/^test/
```

**Find error handlers**:
```
kind:function AND (name~=/error/ OR name~=/catch/ OR name~=/handle/)
```

**Find deprecated code**:
```
name~=/@deprecated/ OR name~=/deprecated/
```

**Find unused exports** (combine with references):
```
kind:export
# Then check references for each
```

---

## Tips & Best Practices

### Indexing Tips

**1. Index after major changes**
```bash
# Via CLI (faster for large changes)
sqry index --force .
```

**2. Watch for stale index warnings**
- Extension shows warnings when index is >24 hours old
- Rebuild if you see "Index may be stale"

**3. Exclude build artifacts**
- Add `.sqry-index` to `.gitignore`
- Index size: ~1-5% of codebase size

**4. Workspace-specific indexes**
- Single-folder workspaces use that folder's index
- Saved `.code-workspace` files can aggregate several source-root indexes
- Use `sqry workspace status <workspace> --json --no-cache` from the CLI to
  inspect the same per-source-root status surface used by VS Code

### Search Tips

**1. Start broad, then narrow**
```
# Broad
kind:function

# Narrow
kind:function AND name~=/auth/

# Specific
kind:function AND name~=/authenticate/ AND async:true
```

**2. Use fuzzy search for exploration**
```
Sqry: Search Workspace... → "autheticate"  # Typo-tolerant!
```

**3. Use structured queries for precision**
```
Sqry: Query... → "kind:function AND name:authenticate"  # Exact match
```

**4. Combine with VS Code search**
- sqry: Find symbol definitions
- VS Code: Find text in comments/strings

### Performance Tips

**1. Limit results for faster queries**
```json
{
  "sqry.limit": 50  // Faster for large codebases
}
```

**2. Use specific queries**
```
# Slow (searches everything)
name~=/./

# Fast (specific filter)
kind:function AND file:./src/auth.ts
```

**3. Close results panel when not needed**
- Saves memory for large result sets

### CodeLens Tips

**1. Disable in large files**
- CodeLens can slow down large files (>5000 lines)
- Disable per-file or globally

**2. Focus on entry points**
- Most useful for public APIs
- Less useful for internal helpers

**3. Use as navigation aid**
- Click caller count to see who calls
- Faster than manual search

---

## Troubleshooting

### Extension Not Working

**Issue**: Extension doesn't activate

**Solutions**:
1. Check VS Code version (need 1.85.0+)
2. Reload window (`Ctrl/Cmd+Shift+P` → `Reload Window`)
3. Check extension logs (`Output` → `sqry`)
4. Reinstall extension

**Issue**: Commands not appearing

**Solutions**:
1. Wait a few seconds after opening workspace
2. Ensure extension is enabled (`Extensions` view)
3. Check for conflicting extensions

### Index Issues

**Issue**: "No index found"

**Solution**: Build index first:
```
Command Palette → Sqry: Index Workspace
```

**Issue**: Indexing times out

**Solutions**:
1. Increase timeout:
   ```json
   {
     "sqry.indexTimeoutMs": 600000  // 10 minutes
   }
   ```
2. Index via CLI (faster):
   ```bash
   sqry index --force .
   ```

**Issue**: "Index is stale"

**Solution**: Rebuild index:
```
Command Palette → Sqry: Index Workspace
```

**Issue**: Missing results

**Solutions**:
1. Rebuild index: `sqry index --force .`
2. Check file is in supported language
3. Verify file is not in `.gitignore`

### Search Issues

**Issue**: No results for known symbol

**Solutions**:
1. Rebuild index (might be stale)
2. Check spelling (use fuzzy search)
3. Try broader query: `name~=/symbol/` instead of `name:symbol`

**Issue**: Too many results

**Solutions**:
1. Add more filters:
   ```
   kind:function AND file:./src/ AND async:true
   ```
2. Reduce limit:
   ```json
   {
     "sqry.limit": 50
   }
   ```

**Issue**: Query syntax error

**Solutions**:
1. Check for typos in keywords (`kind:`, `name:`, etc.)
2. Escape special regex characters: `name~=/foo\.bar/`
3. Use quotes for multi-word: `name:"my function"`

### Performance Issues

**Issue**: Slow queries

**Solutions**:
1. Reduce result limit (`sqry.limit: 50`)
2. Use more specific queries
3. Close results panel when not needed
4. Increase timeout if needed

**Issue**: High memory usage

**Solutions**:
1. Close results panel
2. Reduce `sqry.limit`
3. Rebuild index (might be corrupted)

**Issue**: Extension slow to start

**Solutions**:
1. Disable auto-indexing:
   ```json
   {
     "sqry.autoIndexOnOpen": "never"
   }
   ```
2. Check for large index (>100 MB is unusual)

### Binary Issues

**Issue**: "sqry not found"

**Solutions**:
1. Install sqry CLI: `cargo install --path sqry-cli`
2. Add to PATH, or set full path:
   ```json
   {
     "sqry.path": "/home/user/.cargo/bin/sqry"
   }
   ```
3. Verify: `which sqry` (Linux/Mac) or `where sqry` (Windows)

**Issue**: Wrong sqry version

**Solution**: Update sqry:
```bash
cargo install --path sqry-cli --force
```

---

## More Help

### Documentation

- **Main README**: Project overview and features
- **CHANGELOG**: Version history and updates
- **QUICKSTART**: Getting started with sqry CLI and queries
- **Troubleshooting**: Common issues and solutions

### Support

- **GitHub Issues**: https://github.com/verivus-oss/sqry/issues
- **Discussions**: Ask questions and share tips

### Feedback

This build installs locally via VSIX while we prepare the Marketplace release—feedback is still valuable!

- Report bugs via GitHub Issues
- Suggest features via GitHub Discussions
- Share your workflows and use cases

---

**Last Updated**: 2026-06-13
**Extension Version**: 20.0.3
**sqry Version**: 20.0.3+
