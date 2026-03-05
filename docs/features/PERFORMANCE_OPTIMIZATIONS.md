# Performance Optimizations (Pass 5)

Understanding sqry's graph analysis optimizations.

## Overview

sqry uses **precomputed graph analyses** (called "Pass 5") to make complex graph queries practical on large codebases. These optimizations provide 10-1000x speedups over traditional approaches.

## What is Pass 5?

Pass 5 consists of three precomputed data structures:

1. **SCCs** (Strongly Connected Components)
   - Identifies cycles in the graph
   - Enables O(1) cycle membership checks
   - File: `.sqry/analysis/scc_calls.scc`, `scc_imports.scc`

2. **Condensation DAG**
   - Collapses SCCs into single nodes
   - Creates acyclic graph for efficient traversal
   - File: `.sqry/analysis/cond_calls.dag`, `cond_imports.dag`

3. **2-Hop Interval Labels**
   - Enables fast reachability queries
   - O(|L_out| + |L_in|) per query vs O(V+E)
   - Embedded in condensation DAG files

## Optimized Features

### 1. Cycle Detection

**Command**: `sqry cycles`, `sqry graph is-in-cycle`

**Before Pass 5**:
- Algorithm: Tarjan's SCC detection per query
- Complexity: O(V+E) per query
- Time: Minutes on large graphs

**After Pass 5**:
- Algorithm: SCC membership lookup
- Complexity: O(1) per query
- Time: <1 second on any size graph

**Speedup**: ~1000x

### 2. Path Finding

**Command**: `sqry graph trace-path`

**Before Pass 5**:
- Algorithm: Breadth-first search (BFS)
- Complexity: O(V+E) per path query
- Time: Seconds to minutes

**After Pass 5**:
- Algorithm: BFS with condensation DAG pruning
- Complexity: O(|L_out| + |L_in|) reachability + pruned BFS
- Time: <1 second typical

**Speedup**: 10-100x depending on graph sparsity

### 3. Unused Code Detection

**Command**: `sqry unused`

**Before Pass 5**:
- Algorithm: BFS from each entry point
- Complexity: O(|entries| × (V+E))
- Time: Hours or impractical

**After Pass 5**:
- Algorithm: 2-hop reachability marking
- Complexity: O(|entries| × |SCCs| × |L|)
- Time: 1-3 minutes on large graphs

**Speedup**: 100-1000x

## How It Works

### Strongly Connected Components (SCCs)

**Concept**: Maximal groups of nodes where every node can reach every other node.

**Example**:
```
A → B → C → A    (SCC #1: {A, B, C})
D → E            (SCC #2: {D}, SCC #3: {E})
```

**Benefits**:
- Identify cycles instantly: "Is X in a cycle?" → "Is SCC(X) size > 1?"
- Reduce graph size: Collapse SCCs to single nodes

**Precomputation**: Run once, O(V+E) time using Tarjan's algorithm

### Condensation DAG

**Concept**: DAG where each SCC becomes a single node.

**Example**:
```
Before:                After (condensation):
A → B → C → A         [ABC] → [D] → [E]
  ↓
  D → E
```

**Benefits**:
- Acyclic: Enables topological ordering
- Smaller: Typically 10-100x fewer nodes
- Efficient traversal: No cycles to handle

**Precomputation**: Build from SCCs, O(E) time

### 2-Hop Interval Labels

**Concept**: Assign intervals to nodes so reachability can be checked by interval inclusion.

**Example**:
```
Node A: L_out = [1, 10]    (can reach nodes 1-10)
Node B: L_in = [5, 5]      (is node 5)

Can A reach B? Check: 5 ∈ [1, 10]? → Yes!
```

**Benefits**:
- Fast queries: O(|L_out| + |L_in|) vs O(V+E)
- Compact: ~8 bytes per node average
- Accurate: Exact reachability, not approximate

**Precomputation**: Build from condensation DAG, O(V+E) time

## When Optimizations Apply

### ✅ Optimizations Help

**Cycle Detection**:
- Checking if symbol is in a cycle
- Finding all cycles
- Enumerating cycle members

**Path Finding**:
- Finding if path exists (boolean)
- Finding shortest path
- Finding K shortest paths

**Reachability Analysis**:
- Marking all reachable nodes
- Computing transitive closure
- Unused code detection

### ❌ Optimizations Don't Help

**Single-Hop Queries**:
- Direct callers (`edges_to()`)
- Direct callees (`edges_from()`)
- Already O(1) with adjacency lists

**Exhaustive Enumeration**:
- Finding ALL paths (not just one)
- Exploring ALL nodes within distance N
- No pruning opportunity

**String-Based Searches**:
- Pattern matching
- Name searches
- No graph traversal involved

## Performance Characteristics

### Time Complexity

| Operation | Without Pass 5 | With Pass 5 | Speedup |
|-----------|----------------|-------------|---------|
| Is in cycle | O(V+E) | O(1) | ~1000x |
| Find path | O(V+E) BFS | O(\|L\|) + pruned BFS | 10-100x |
| Reachability | O(\|entries\| × (V+E)) | O(\|entries\| × \|SCCs\| × \|L\|) | 100-1000x |
| Direct callers | O(1) | O(1) | 1x |

### Space Complexity

**Per Graph**:
- SCCs: ~4 bytes per node
- Condensation DAG: ~40 bytes per SCC
- 2-hop labels: ~8 bytes per node average

**Example (384K nodes)**:
- SCCs: ~1.5 MB
- Condensation: ~400 KB (10K SCCs)
- Labels: ~3 MB
- **Total**: ~5 MB overhead

**Typical overhead**: 1-2% of graph size

### Precomputation Cost

**One-time cost** (per graph version):
- SCC detection: O(V+E) using Tarjan's
- Condensation build: O(E)
- 2-hop labeling: O(V+E)
- **Total**: O(V+E), typically 1-5 seconds

**When to recompute**:
- After `sqry index` (graph changes)
- Automatically on first query if missing
- Can trigger manually with `sqry graph analyze` (if command exists)

## Real-World Performance

### sqry Codebase (Self-Test)

**Graph Stats**:
- Nodes: 384,133
- Edges: 1,312,440
- Files: 1,234
- SCCs: ~10,000

**Results**:
- `is-in-cycle`: <1 second ✅
- `trace-path`: <1 second ✅
- `unused`: 2m 8s ✅

### Linux Kernel Benchmark (Large-Scale)

From the documented Linux benchmark run (`docs/launch/TECHNICAL_ARTICLE.md`):

- Indexed commit: `a75cb869a8ccc88b0bc7a44e1597d9c7995c56e5`
- Corpus size: ~70,000 files (C-dominant mixed-language tree)
- Graph size: 11,205,544 nodes, 18,292,255 resolved edges
- Snapshot size: 1.8 GB (`.sqry/graph/snapshot.sqry`)
- `sqry index`: ~1m48s on a 24-core machine
- `sqry graph direct-callers printk --limit 100`: ~85ms

Interpretation:
- Indexing is practical for deliberate preprocessing on kernel-scale code.
- Graph queries remain interactive after index build, especially bounded
  relation queries (`direct-callers`, `direct-callees`).
- Query shape matters; broad scans remain materially slower than targeted
  graph queries.

### Typical Performance by Size

| Codebase Size | SCC Compute | Unused Code | Cycle Check | Path Find |
|---------------|-------------|-------------|-------------|-----------|
| Small (<10K) | <1s | <5s | <0.1s | <0.5s |
| Medium (10-100K) | 1-5s | 10-30s | <0.1s | <2s |
| Large (100-500K) | 5-15s | 1-3m | <0.1s | <5s |
| Very Large (>500K) | 15-60s | 3-10m | <0.1s | <10s |

## Limitations

### 1. Precomputation Required

**Limitation**: Analyses must be precomputed before queries are fast.

**Impact**:
- First query after index change may be slower
- Graph changes invalidate analyses
- Requires disk space for analysis files

**Mitigation**: Analyses auto-generate on first use

### 2. Memory Overhead

**Limitation**: 2-hop labels require additional memory.

**Impact**:
- ~1-2% of graph size
- ~10-30 MB for typical large codebases
- Loaded on-demand, not kept permanently

**Mitigation**: Labels are memory-efficient (interval compression)

### 3. Dynamic Code

**Limitation**: Only analyzes static call graph.

**Impact**:
- Cannot detect paths through dynamic dispatch
- Virtual calls, function pointers not resolved
- Reflection, eval, dynamic loading not tracked

**Mitigation**: Conservative analysis (may overestimate reachability)

### 4. Incremental Updates

**Limitation**: Currently requires full recomputation on graph changes.

**Impact**:
- Any code change invalidates analyses
- ~1-60 seconds to recompute (depending on size)

**Future work**: Incremental SCC updates

## Best Practices

### 1. Keep Index Up-to-Date

Run `sqry index` after code changes:

```bash
# After pulling code
git pull && sqry index

# After major refactoring
sqry index --force
```

### 2. Monitor Analysis Size

Check disk usage periodically:

```bash
du -sh .sqry/analysis/
```

Typical: 1-5% of source code size.

### 3. Use Optimized Features

Prefer optimized operations:

```bash
# ✅ Fast: Uses Pass 5
sqry graph is-in-cycle <symbol>

# ❌ Slower: Query-time check (if used)
sqry graph cycles | grep <symbol>
```

### 4. Understand Tradeoffs

**Fast Operations** (O(1) or O(|L|)):
- Cycle membership
- Reachability checks
- Path existence

**Slower Operations** (O(V+E) or worse):
- Enumerating all paths
- Exhaustive traversal
- Complex filtering

## FAQ

**Q: Do I need to manually trigger analysis?**
A: No. Analyses are automatically generated on first use after indexing.

**Q: How do I know if Pass 5 is being used?**
A: If cycle checks return in <1 second on large graphs, Pass 5 is active. Check for `.sqry/analysis/*.scc` files.

**Q: What if analyses are out of date?**
A: sqry validates analyses against the graph. If mismatched, it falls back to query-time computation and warns you.

**Q: Can I disable Pass 5?**
A: Not currently. It's always used when available. Falls back gracefully if not.

**Q: Why is unused code detection still slow?**
A: Reachability marking is O(|entries| × |SCCs| × |L|). With 100+ entry points, this is still expensive. Future optimization: cache reachable SCCs.

**Q: Does Pass 5 work with all languages?**
A: Yes. It's language-agnostic and works on the unified code graph.

## See Also

- [Unused Code Detection](UNUSED_CODE_DETECTION.md)
- [Cycle Detection](CYCLE_DETECTION.md)
- [Call Path Tracing](CALL_PATHS.md)
