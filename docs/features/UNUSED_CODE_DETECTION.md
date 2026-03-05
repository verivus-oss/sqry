# Unused Code Detection

Find dead code in your codebase using static reachability analysis.

## Overview

The `unused` command identifies symbols (functions, classes, variables) that are never used in your codebase. It works by:

1. Identifying entry points (main functions, public exports, tests)
2. Marking all code reachable from these entry points
3. Reporting everything else as unused

## Quick Start

```bash
# Find all unused code
sqry unused

# Find unused public APIs
sqry unused --scope public

# Find unused functions only
sqry unused --scope function

# Find unused code in Rust files
sqry unused --lang rust
```

## Usage

```bash
sqry unused [OPTIONS] [PATH]
```

### Options

**Scope Filtering**:
- `--scope all` - All unused symbols (default)
- `--scope public` - Public symbols with no external references
- `--scope private` - Private symbols with no references
- `--scope function` - Unused functions/methods only
- `--scope struct` - Unused structs/classes/types only

**Language Filtering**:
- `--lang <LANG>` - Filter by programming language (e.g., `rust`, `python`, `javascript`)

**Symbol Kind Filtering**:
- `--kind <KIND>` - Filter by symbol kind (e.g., `function`, `class`, `variable`)

**Output Control**:
- `--max-results <N>` - Limit results (default: 100)
- `--json` - Output as JSON
- `--csv` - Output as CSV
- `--preview [N]` - Show code context (default: 3 lines)

## How It Works

### Entry Point Detection

The tool uses language-specific heuristics to identify code entry points:

#### Rust
- `main()` functions
- Public items in `lib.rs` or `main.rs`
- All symbols with `pub` visibility that have export edges
- Test functions (annotated with `#[test]`)

#### Python
- Functions and classes in `__init__.py`
- Top-level functions (common `if __name__ == "__main__"` pattern)
- All classes (may be used via reflection)

#### JavaScript/TypeScript
- Symbols with `export` statements
- Top-level functions and classes
- React components

#### Java
- `public static void main()` methods
- Public classes

#### Go
- `func main()`
- Exported symbols (capitalized names with public visibility)

#### C/C++
- `main()` function
- Non-private functions

#### Generic Fallback
- Any `main()` function
- Symbols with export edges
- Public visibility symbols

### Reachability Analysis

Once entry points are identified, the tool:

1. Loads precomputed graph analyses (Pass 5: SCC + 2-hop labels)
2. Marks all symbols reachable from entry points
3. Reports unreachable symbols as unused

**Performance**: Uses 2-hop interval labeling for O(|entries| × |SCCs| × |labels|) complexity, making it practical for large codebases.

## Examples

### Example 1: Find Unused Public APIs

Identify public functions/types that nothing outside your crate uses:

```bash
sqry unused --scope public --lang rust
```

**Use case**: Finding APIs you can safely deprecate or remove.

### Example 2: Dead Code After Refactoring

After a major refactoring, find leftover code:

```bash
sqry unused --scope private --preview 5
```

**Use case**: Cleanup after feature removal or code reorganization.

### Example 3: Language-Specific Cleanup

Focus on a specific language:

```bash
sqry unused --lang python --kind function
```

**Use case**: Python-specific dead code detection.

### Example 4: Export for Review

Generate a CSV report for team review:

```bash
sqry unused --csv --headers > unused_code_report.csv
```

**Use case**: Code review or technical debt tracking.

## Understanding Results

### Output Format

**Text Output**:
```
sqry-mcp/src/old_helper.rs:42
  unused_helper
  function · private · rust
```

**JSON Output**:
```json
{
  "file": "sqry-mcp/src/old_helper.rs",
  "count": 1,
  "symbols": [{
    "name": "unused_helper",
    "qualified_name": "old_helper::unused_helper",
    "kind": "Function",
    "file": "sqry-mcp/src/old_helper.rs",
    "line": 42,
    "language": "rust",
    "visibility": "private"
  }]
}
```

### Symbol Kinds

- **Function**: Regular functions
- **Method**: Class/struct methods
- **Class**: Classes, structs, interfaces
- **Variable**: Variables, constants
- **Type**: Type aliases, type definitions
- **Trait**: Rust traits, Go interfaces
- **Module**: Modules, packages

## Limitations

### 1. Dynamic Code References

The tool cannot detect usage through:
- **Reflection**: `getattr()`, `eval()`, `__import__()`
- **Dynamic loading**: `dlopen()`, `LoadLibrary()`
- **String-based references**: Configuration files, CLI arguments
- **Serialization**: JSON/XML schemas referencing fields

**Mitigation**: Manually review results before deletion. The tool is conservative but not perfect.

### 2. Entry Point Heuristics

Language-specific heuristics may miss:
- **Custom test frameworks**: Non-standard test runners
- **Build-time code generation**: Macros, code generators
- **Framework conventions**: Specific framework entry points
- **Plugin systems**: Dynamically loaded plugins

**Mitigation**: Results may include false positives (marked as unused when actually used). Always verify before deleting.

### 3. Cross-Language References

Limited detection of:
- **FFI boundaries**: Rust → C, Python → C
- **RPC/API endpoints**: HTTP handlers, gRPC services
- **Configuration-driven code**: Dependency injection, service locators

**Mitigation**: Use `--scope public` to be more conservative.

### 4. Test Code

May not detect:
- Test utilities used only in tests
- Test fixtures and helpers
- Benchmark code

**Mitigation**: Test code is typically marked as public or has special annotations, so entry point detection should catch it.

## Best Practices

### 1. Start Conservative

Begin with public scope to find obviously unused APIs:

```bash
sqry unused --scope public
```

Review and remove these first, then move to private scope.

### 2. Use Preview Mode

Always review context before deletion:

```bash
sqry unused --preview 5
```

Understand why the code exists before removing it.

### 3. Check Git History

Before deleting, check when it was last modified:

```bash
git log --oneline path/to/unused_file.rs
```

Recently modified code might be work-in-progress.

### 4. Language-Specific Passes

Run per language for focused cleanup:

```bash
sqry unused --lang rust --scope private
sqry unused --lang python --scope function
```

### 5. Track Over Time

Run periodically and track results:

```bash
sqry unused --json > unused_$(date +%Y%m%d).json
```

Monitor dead code accumulation.

### 6. Integrate with CI

Add as a CI check to prevent dead code accumulation:

```bash
# Fail if more than 10 unused public symbols
UNUSED_COUNT=$(sqry unused --scope public --json | jq '. | map(.count) | add')
if [ "$UNUSED_COUNT" -gt 10 ]; then
  echo "Too much unused public code: $UNUSED_COUNT symbols"
  exit 1
fi
```

## Performance

### Expected Performance

| Codebase Size | Typical Time | Notes |
|---------------|--------------|-------|
| Small (<10K nodes) | <5 seconds | Very fast |
| Medium (10-100K nodes) | 10-30 seconds | Fast |
| Large (100-500K nodes) | 1-3 minutes | Practical |
| Very Large (>500K nodes) | 3-10 minutes | Still usable |

**Factors affecting performance**:
- Number of entry points (more entry points = more reachability checks)
- Graph density (more edges = more traversal)
- Number of SCCs (strongly connected components)
- Available memory for 2-hop labels

### Prerequisites

For best performance, ensure:

1. **Index is built**: `sqry index`
2. **Graph is up-to-date**: Run `sqry index` after code changes
3. **Sufficient memory**: ~10-30 MB overhead for analyses

### Optimization Tips

**Speed up repeated runs**:
- Results are cached per graph version
- No need to rebuild index if code hasn't changed
- Use `--max-results` to limit output processing

**Memory usage**:
- Peak memory: ~2-3x graph size
- For 384K node graph: ~800 MB total
- Analyses load on-demand, not kept in memory

## Troubleshooting

### "Pass 5 analyses not available"

**Problem**: Reachability analysis requires precomputed graph data.

**Solution**: The analyses should be automatically available. If not:
1. Check `.sqry/graph/` directory exists
2. Verify `.sqry/analysis/` directory has `.scc` and `.dag` files
3. Try rebuilding: `sqry index --force`

### "No symbols found in file"

**Problem**: The specified file isn't indexed or has no symbols.

**Solution**:
1. Verify file exists: `ls path/to/file.rs`
2. Check it's a supported language
3. Rebuild index: `sqry index --force`

### Results Seem Wrong

**Problem**: Marked as unused but you know it's used.

**Possible causes**:
1. Dynamic references (reflection, string-based lookup)
2. External crate usage (not visible in current codebase)
3. Missing entry point (custom test runner, framework convention)

**Solution**:
- Verify manually before deleting
- Check callers: `sqry graph direct-callers <symbol>`
- Search for string references: `rg "symbol_name"`

### Performance is Slow

**Problem**: Takes >5 minutes on medium codebase.

**Possible causes**:
1. Very large graph (>1M nodes)
2. Many entry points (>1000)
3. Dense graph (average degree >10)

**Solution**:
- Use `--max-results` to limit processing
- Filter by language: `--lang rust`
- Check graph stats: `sqry graph stats`

## FAQ

**Q: Can I configure custom entry points?**
A: Not yet. Entry points are detected automatically using language-specific heuristics. Feature planned for future release.

**Q: Does it work with all languages?**
A: Works best with Rust, Python, JavaScript/TypeScript, Java, Go, C/C++. Other languages use generic fallback heuristics.

**Q: Is it safe to delete everything marked as unused?**
A: No. Always review results manually. The tool cannot detect dynamic references, reflection, or framework conventions.

**Q: How often should I run it?**
A: Depends on your workflow:
- Weekly: Regular cleanup
- After major refactoring: Find leftover code
- Before releases: Clean up dead code
- In CI: Prevent accumulation

**Q: Can it detect unused test code?**
A: Test functions are treated as entry points, so their dependencies are marked as reachable. Orphaned test utilities may be detected.

**Q: What about unused imports?**
A: Use your language's native tools (e.g., `cargo fix --allow-unused-imports` for Rust). This tool focuses on unused definitions, not imports.

**Q: Does it handle macros/generics correctly?**
A: Generally yes, but macro-generated code and generic instantiations may cause false positives. Review carefully.

## See Also

- [Cycle Detection](CYCLE_DETECTION.md) - Finding circular dependencies
- [Call Paths](CALL_PATHS.md) - Tracing execution paths
- [Performance Optimizations](PERFORMANCE_OPTIMIZATIONS.md) - Pass 5 analysis details
