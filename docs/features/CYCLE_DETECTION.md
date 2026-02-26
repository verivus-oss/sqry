# Cycle Detection

Find circular dependencies in your codebase.

## Quick Start

```bash
# Find all call cycles
sqry cycles

# Find import cycles
sqry cycles --type imports

# Check if specific symbol is in a cycle
sqry graph is-in-cycle build_graph --show-cycle
```

## Overview

Circular dependencies can lead to:
- Initialization order issues
- Tight coupling
- Difficult refactoring
- Compilation problems (in some languages)

sqry detects cycles using **precomputed strongly connected components (SCCs)**, making detection fast even on large codebases.

## Commands

### Find All Cycles

```bash
sqry cycles [OPTIONS]
```

**Options**:
- `--type <TYPE>` - Cycle type: `calls`, `imports`, `modules` (default: `calls`)
- `--min-size <N>` - Minimum cycle size (default: 2)
- `--max-results <N>` - Limit results (default: 100)
- `--json` - Output as JSON

**Examples**:
```bash
# Find call cycles
sqry cycles --type calls

# Find import cycles (common in Python/JS)
sqry cycles --type imports

# Find large cycles only (5+ symbols)
sqry cycles --min-size 5
```

### Check Specific Symbol

```bash
sqry graph is-in-cycle <SYMBOL> [OPTIONS]
```

**Options**:
- `--cycle-type <TYPE>` - Type: `calls`, `imports`, `modules` (default: `calls`)
- `--show-cycle` - Display the full cycle path
- `--json` - Output as JSON

**Examples**:
```bash
# Check if function is in a cycle
sqry graph is-in-cycle process_request

# Show the full cycle path
sqry graph is-in-cycle UserService --show-cycle

# Check import cycles
sqry graph is-in-cycle utils --cycle-type imports
```

## Understanding Results

### Cycle Output

**Text format**:
```
Cycle of size 3:
  src/user.rs:42  UserService::authenticate
  src/auth.rs:15  AuthManager::verify_token
  src/user.rs:89  UserService::get_user_by_token
```

**JSON format**:
```json
{
  "cycles": [{
    "size": 3,
    "symbols": [
      {"name": "UserService::authenticate", "file": "src/user.rs", "line": 42},
      {"name": "AuthManager::verify_token", "file": "src/auth.rs", "line": 15},
      {"name": "UserService::get_user_by_token", "file": "src/user.rs", "line": 89}
    ]
  }]
}
```

### Cycle Types

**Call Cycles**:
- `A() calls B() calls C() calls A()`
- Most common type
- May indicate design issues

**Import Cycles**:
- `module A imports B imports C imports A`
- Common in Python, JavaScript
- Can cause initialization issues

**Module Cycles**:
- Module-level dependency cycles
- May prevent clean architecture

## Breaking Cycles

### Strategy 1: Extract Interface

**Before**:
```rust
// user.rs
impl UserService {
    fn authenticate() {
        auth_manager.verify_token()  // Calls auth module
    }
}

// auth.rs
impl AuthManager {
    fn verify_token() {
        user_service.get_user()  // Calls back to user!
    }
}
```

**After**:
```rust
// user.rs
trait UserLookup {
    fn get_user(&self, id: UserId) -> User;
}

impl UserService {
    fn authenticate() {
        auth_manager.verify_token()
    }
}

// auth.rs
impl AuthManager {
    fn verify_token(&self, lookup: &dyn UserLookup) {
        let user = lookup.get_user(id);  // Uses trait, no direct dependency
    }
}
```

### Strategy 2: Introduce Mediator

Move cyclic logic to a third module:

```rust
// user.rs - no dependency on auth
// auth.rs - no dependency on user
// user_auth.rs - depends on both, no cycle
```

### Strategy 3: Invert Dependency

Make the lower-level module depend on an interface from the higher-level module:

```
Before: A → B → C → A
After:  A → B → C
             ↑
        Interface defined in B
```

## Performance

**Detection Speed**:
- **With Pass 5**: O(1) per symbol (precomputed SCCs)
- **Without Pass 5**: O(V+E) per query

**Typical Performance**:
| Graph Size | Check Single Symbol | Find All Cycles |
|------------|-------------------|-----------------|
| <10K nodes | <0.1s | <5s |
| 10-100K nodes | <0.1s | <30s |
| 100-500K nodes | <0.1s | 1-3m |

**Why is "find all cycles" slower?**
- Must enumerate all cycles (not just detect presence)
- Format and deduplicate results
- Post-processing overhead

**Why is "check single symbol" fast?**
- Uses precomputed SCC membership
- O(1) lookup: just check which SCC the symbol belongs to
- No graph traversal needed

## Best Practices

### 1. Check Before Refactoring

Before major refactoring, identify existing cycles:

```bash
sqry cycles --type calls --json > cycles_before.json
```

After refactoring, verify they're resolved.

### 2. Monitor in CI

Prevent new cycles from being introduced:

```bash
# Fail if new call cycles are found
CYCLES=$(sqry cycles --json | jq '.cycles | length')
if [ "$CYCLES" -gt 0 ]; then
  echo "Circular dependencies detected: $CYCLES cycles"
  exit 1
fi
```

### 3. Focus on Large Cycles First

Large cycles are often easiest to break:

```bash
sqry cycles --min-size 5
```

### 4. Use Visualization

Export cycles as a graph for visualization:

```bash
sqry export --format dot --filter cycles > cycles.dot
dot -Tpng cycles.dot -o cycles.png
```

## Limitations

### 1. Indirect Cycles

May not detect cycles through:
- Dynamic dispatch (trait objects, interfaces)
- Function pointers
- Callback registrations

### 2. Conditional Cycles

Cycles that only exist in certain conditions:

```rust
#[cfg(feature = "auth")]
fn process() { other_module::helper() }
```

### 3. Type-Level Cycles

Generic type dependencies that form cycles:

```rust
struct A<T> { b: B<T> }
struct B<T> { a: A<T> }  // Type cycle, not call cycle
```

## See Also

- [Unused Code Detection](UNUSED_CODE_DETECTION.md)
- [Call Path Tracing](CALL_PATHS.md)
- [Graph Analysis Guide](../guides/GRAPH_ANALYSIS.md)
