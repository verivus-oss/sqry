# Call Path Tracing

Find execution paths between symbols in your codebase.

## Quick Start

```bash
# Find path from main to a function
sqry graph trace-path main process_request

# Find paths between two functions
sqry graph trace-path authenticate validate_token

# Export as JSON for analysis
sqry graph trace-path main handle_error --json
```

## Overview

Call path tracing helps you:
- **Understand code flow**: How does execution reach this function?
- **Impact analysis**: What calls this critical function?
- **Debugging**: Trace potential execution paths
- **Refactoring**: Understand dependencies before changes

sqry uses **Pass 5 condensation DAG pruning** to find paths efficiently, even in large codebases.

## Usage

```bash
sqry graph trace-path <FROM> <TO> [OPTIONS]
```

### Options

- `--languages <LANGS>` - Filter by languages (comma-separated)
- `--full-paths` - Show full file paths
- `--json` - Output as JSON
- `--csv` - Output as CSV

### Examples

**Basic path finding**:
```bash
sqry graph trace-path main process_request
```

**Cross-language paths**:
```bash
sqry graph trace-path api_handler database_query
```

**JSON output for tools**:
```bash
sqry graph trace-path main error_handler --json | jq '.paths[0].steps'
```

## Understanding Results

### Path Output

**Text format**:
```
Path 1 (3 steps):
  main (src/main.rs:10)
    → calls →
  run_server (src/server.rs:45)
    → calls →
  handle_request (src/handlers.rs:23)
```

**JSON format**:
```json
{
  "from_symbol": "main",
  "to_symbol": "handle_request",
  "paths": [{
    "steps": [
      {
        "symbol": {"name": "main", "file": "src/main.rs", "line": 10},
        "edge_type": "start"
      },
      {
        "symbol": {"name": "run_server", "file": "src/server.rs", "line": 45},
        "edge_type": "call",
        "confidence": 1.0
      },
      {
        "symbol": {"name": "handle_request", "file": "src/handlers.rs", "line": 23},
        "edge_type": "call",
        "confidence": 1.0
      }
    ],
    "length": 2,
    "cross_language": false,
    "score": 0.5
  }],
  "total": 1
}
```

### Edge Types

- **call**: Regular function call
- **async_call**: Asynchronous function call
- **import**: Module import (Python, JS)
- **reference**: Reference to symbol
- **inherits**: Class inheritance
- **implements**: Interface implementation

### Confidence Scores

Indicates reliability of the edge:
- **1.0**: Direct, synchronous call
- **0.9**: Async call
- **0.95**: Import (non-wildcard)
- **0.8**: Reference (may or may not be called)
- **0.7**: Wildcard import

## Use Cases

### 1. Impact Analysis

**Question**: What code paths lead to this critical function?

```bash
sqry graph trace-path main delete_user
```

**Use**: Understand all ways `delete_user` can be invoked.

### 2. Debugging

**Question**: How does execution reach this error handler?

```bash
sqry graph trace-path main handle_database_error
```

**Use**: Trace error propagation paths.

### 3. Refactoring Safety

**Question**: What depends on this function I want to change?

```bash
# Find all callers first
sqry graph direct-callers legacy_function

# Then trace paths from entry points
sqry graph trace-path main legacy_function
```

**Use**: Understand downstream impact of changes.

### 4. Performance Investigation

**Question**: Why is this rarely-used function showing up in profiles?

```bash
sqry graph trace-path main expensive_operation
```

**Use**: Find unexpected call paths.

### 5. Security Audit

**Question**: Can user input reach this sensitive function?

```bash
sqry graph trace-path handle_user_input execute_system_command
```

**Use**: Identify potential security risks.

## Advanced Features

### Cross-Language Paths

Automatically detects paths across language boundaries:

```bash
sqry graph trace-path http_handler sql_query
```

**Example output**:
```
Path 1 (cross-language):
  http_handler (src/api.ts:10) [TypeScript]
    → FFI call →
  process_data (src/processor.rs:45) [Rust]
    → calls →
  sql_query (src/db.rs:89) [Rust]
```

### Multiple Paths

If multiple paths exist, returns shortest paths:

```bash
sqry graph trace-path main target --json | jq '.paths | length'
```

Default: Returns top 5 shortest paths.

### Path Scoring

Paths are scored by:
- **Length**: Shorter is better
- **Confidence**: Higher confidence is better
- **Cross-language bonus**: Novel paths get slight boost

## Performance

**Path Finding Speed**:

| Graph Size | No Path | Path Exists (short) | Path Exists (long) |
|------------|---------|--------------------|--------------------|
| <10K nodes | <0.1s | <0.5s | <2s |
| 10-100K nodes | <0.5s | <2s | <10s |
| 100-500K nodes | <1s | <5s | <30s |

**Why is it fast?**
- **Pass 5 pruning**: Skips branches that can't reach target
- **Early termination**: Stops after finding paths
- **SCC shortcuts**: Uses precomputed strongly connected components

**Without Pass 5**:
- Must explore entire graph to prove no path exists
- No pruning → explores all branches
- 10-100x slower on large graphs

## Limitations

### 1. Static Analysis Only

Cannot detect paths through:
- **Dynamic dispatch**: Trait objects, interfaces, virtual calls
- **Function pointers**: Callbacks, closures stored in variables
- **Reflection**: `getattr()`, `eval()`, dynamic loading

**Example (may miss)**:
```rust
let handler: fn() = if condition { foo } else { bar };
handler();  // Static analysis may not know which function is called
```

### 2. Path Existence ≠ Execution

Finding a path doesn't mean:
- The path is ever actually taken at runtime
- The code is reachable (may be behind dead condition)
- The path is the only way to reach the target

**Example**:
```rust
fn target() { }
fn never_called() { target(); }  // Path exists but never executed
```

### 3. Async Boundaries

May not accurately trace through:
- Async/await boundaries
- Event loops
- Message queues
- Thread spawns

### 4. Conditional Compilation

Paths may depend on:
- Feature flags (`#[cfg(feature = "...")]`)
- Platform-specific code (`#[cfg(target_os = "...")]`)
- Build configurations

## Best Practices

### 1. Start from Entry Points

Always trace from well-known entry points:

```bash
sqry graph trace-path main <target>  # Good
sqry graph trace-path <random> <target>  # Less useful
```

### 2. Verify with Debugging

Static analysis finds potential paths, not actual execution:

```bash
# Find potential paths
sqry graph trace-path main bug_location

# Verify with debugger or logging
```

### 3. Use with Callers/Callees

Combine with direct relationship queries:

```bash
# Find all direct callers
sqry graph direct-callers sensitive_function

# Trace paths from entry points to each caller
for caller in $(sqry graph direct-callers sensitive_function --json | jq -r '.[]'); do
  sqry graph trace-path main "$caller"
done
```

### 4. Export for Visualization

Generate visual graphs:

```bash
# Export subgraph around a path
sqry graph trace-path main target --json | \
  jq '.paths[0].steps | .[].symbol.name' | \
  xargs -I {} sqry export --symbol {} --format dot
```

### 5. CI Integration

Check for unwanted paths:

```bash
# Ensure UI code never directly calls database
if sqry graph trace-path render_ui db_execute --json | jq -e '.paths | length > 0'; then
  echo "ERROR: UI code has direct path to database!"
  exit 1
fi
```

## Troubleshooting

### "No path found"

**Possible reasons**:
1. **Truly no path exists**: Functions are in disconnected components
2. **Symbol not found**: Typo in symbol name
3. **Indirect path**: Path goes through dynamic dispatch (not detected)
4. **Conditional compilation**: Path only exists in certain build configs

**Solutions**:
- Check symbol names: `sqry search <symbol>`
- Check direct connections: `sqry graph direct-callees <from>`
- Try broader search: Find intermediate symbols

### Results seem incomplete

**Possible reasons**:
1. **Dynamic dispatch**: Virtual calls, trait objects
2. **Macro expansion**: Macro-generated calls
3. **Cross-language gaps**: FFI boundaries not fully mapped

**Solutions**:
- Check call hierarchy: `sqry graph call-hierarchy <symbol>`
- Manual code review for dynamic dispatch
- Use debugger to verify actual paths

### Performance is slow

**Possible reasons**:
1. **Very long paths**: Max hops too high (default: 5)
2. **Dense graph**: High average degree
3. **Many paths**: Exploring many alternatives

**Solutions**:
- Reduce max hops (not configurable in CLI yet)
- Filter by language: `--languages rust`
- Check graph density: `sqry graph stats`

## See Also

- [Cycle Detection](CYCLE_DETECTION.md) - Find circular dependencies
- [Unused Code Detection](UNUSED_CODE_DETECTION.md) - Find dead code
- [Performance Optimizations](PERFORMANCE_OPTIMIZATIONS.md) - Pass 5 analysis details
