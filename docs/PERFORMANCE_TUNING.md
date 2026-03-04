# Performance Tuning Guide

This guide covers sqry's performance-related configuration, optimization strategies, and tuning recommendations for different workload profiles.

## Table of Contents

- [Environment Variables Reference](#environment-variables-reference)
- [Cache Configuration](#cache-configuration)
- [Buffer Tuning](#buffer-tuning)
- [Search Configuration](#search-configuration)
- [Index Optimization](#index-optimization)
- [Graph Analysis Performance](#graph-analysis-performance)
- [SIMD Acceleration](#simd-acceleration)
- [Memory Management](#memory-management)
- [Network Filesystem Detection](#network-filesystem-detection)
- [Benchmarking](#benchmarking)
- [Workload Profiles](#workload-profiles)

---

## Environment Variables Reference

All performance-related settings are configurable via environment variables. Defaults are tuned for typical workloads; override only when profiling reveals a bottleneck.

### Cache

| Variable | Default | Range | Purpose |
|----------|---------|-------|---------|
| `SQRY_CACHE_MAX_BYTES` | 50 MB | — | Maximum cache size in bytes |
| `SQRY_CACHE_ROOT` | `.sqry-cache` | — | Cache directory location |
| `SQRY_CACHE_DISABLE_PERSIST` | `0` | `0`/`1` | Disable disk persistence |
| `SQRY_CACHE_POLICY` | `lru` | `lru`/`tiny_lfu`/`hybrid` | Eviction policy |
| `SQRY_CACHE_POLICY_WINDOW` | `0.20` | 0.05–0.95 | Protected window ratio (TinyLFU/hybrid) |

### Buffers

| Variable | Default | Range | Purpose |
|----------|---------|-------|---------|
| `SQRY_READ_BUFFER` | 8 KB | 1 KB – 1 MB | Read buffer for file I/O |
| `SQRY_WRITE_BUFFER` | 8 KB | 1 KB – 1 MB | Write buffer for file I/O |
| `SQRY_PARSE_BUFFER` | 64 KB | 4 KB – 10 MB | Buffer for tree-sitter parsing |
| `SQRY_INDEX_BUFFER` | 1 MB | 64 KB – 100 MB | Buffer for index serialization |
| `SQRY_MMAP_THRESHOLD` | 10 MB | 1 MB – 1 GB | File size threshold for memory-mapped I/O |

### Search

| Variable | Default | Purpose |
|----------|---------|---------|
| `SQRY_FUZZY_USE_JACCARD` | `1` | Fuzzy search mode (`1`=Jaccard, `0`=ratio) |
| `SQRY_FALLBACK_ENABLED` | `true` | Enable text search fallback |
| `SQRY_MIN_SEMANTIC_RESULTS` | `1` | Semantic result threshold before fallback |
| `SQRY_TEXT_CONTEXT_LINES` | `2` | Context lines for text fallback results |
| `SQRY_MAX_TEXT_RESULTS` | `1000` | Maximum text search results |
| `SQRY_SHOW_SEARCH_MODE` | `true` | Display which search mode was used |

### Git Backend

| Variable | Default | Purpose |
|----------|---------|---------|
| `SQRY_GIT_BACKEND` | `auto` | Git backend: `auto`, `subprocess`, `none` |
| `SQRY_GIT_INCLUDE_UNTRACKED` | `true` | Include untracked files in analysis |
| `SQRY_GIT_RENAME_SIMILARITY` | `50` | Similarity threshold for rename detection (0–100) |
| `SQRY_GIT_MAX_OUTPUT_SIZE` | 10 MB | Max git output size (1 MB – 100 MB) |
| `SQRY_GIT_TIMEOUT_MS` | `3000` | Git command timeout in milliseconds |

### Safety Limits

| Variable | Default | Range | Purpose |
|----------|---------|-------|---------|
| `SQRY_MAX_SOURCE_FILE_SIZE` | 50 MB | 1 MB – 500 MB | Files larger than this are rejected during indexing |
| `SQRY_MAX_REPOSITORIES` | 1,000 | 10 – 10,000 | Limits workspace scanning scope |
| `SQRY_WATCH_EVENT_QUEUE` | 10,000 | 100 – 100,000 | Watch mode event queue capacity |
| `SQRY_MAX_QUERY_LENGTH` | 10 KB | 1 KB – 100 KB | Maximum query string length |
| `SQRY_MAX_PREDICATES` | 100 | 10 – 1,000 | Maximum predicates per query |

---

## Cache Configuration

sqry uses a two-layer cache (in-memory policy + persisted `.sqry-cache/` entries) with configurable eviction policies.

### Eviction Policies

**LRU** (default): Least-recently-used eviction. Best for general workloads where recent queries predict future queries.

**TinyLFU**: Windowed admission with a protected hot set. Best for workloads with a stable "hot" set of frequently accessed symbols — the admission filter rejects one-off entries that would evict hot items.

**Hybrid**: LRU window + TinyLFU protected region. A compromise that uses LRU for recent entries and TinyLFU for the main cache body.

```bash
# Switch to TinyLFU for large codebases with repeated queries
export SQRY_CACHE_POLICY=tiny_lfu

# Increase cache size for large monorepos
export SQRY_CACHE_MAX_BYTES=$((200 * 1024 * 1024))  # 200 MB

# Tune the TinyLFU protected window (default: 20%)
export SQRY_CACHE_POLICY_WINDOW=0.30
```

### Cache Lifecycle

```bash
# View cache statistics
sqry cache stats
sqry cache stats --json

# Prune old entries
sqry cache prune --days 30

# Cap cache to a size limit
sqry cache prune --size 1GB

# Preview before deleting
sqry cache prune --days 7 --dry-run

# Clear everything
sqry cache clear --confirm
```

### Custom Cache Location

```bash
# Use a fast local disk for cache
export SQRY_CACHE_ROOT=/tmp/sqry-cache

# Or a shared location across projects
export SQRY_CACHE_ROOT=$HOME/.sqry/cache
```

### Disabling Persistence

For ephemeral environments (CI, containers) where disk persistence adds overhead:

```bash
export SQRY_CACHE_DISABLE_PERSIST=1
```

---

## Buffer Tuning

Buffer sizes control the trade-off between memory usage and I/O efficiency. The defaults are tuned for typical workloads; adjust only when profiling shows I/O as a bottleneck.

### When to Increase Buffers

- **Large files**: Increase `SQRY_PARSE_BUFFER` for codebases with many large source files
- **Slow storage**: Increase `SQRY_READ_BUFFER` and `SQRY_WRITE_BUFFER` to reduce syscall overhead
- **Large indexes**: Increase `SQRY_INDEX_BUFFER` for faster serialization/deserialization

### When to Decrease Buffers

- **Memory-constrained environments**: Reduce buffer sizes in CI runners or containers with limited RAM
- **Many concurrent processes**: Lower buffers to reduce per-process memory footprint

```bash
# Example: large codebase on SSD
export SQRY_READ_BUFFER=65536    # 64 KB
export SQRY_PARSE_BUFFER=262144  # 256 KB
export SQRY_INDEX_BUFFER=4194304 # 4 MB

# Example: memory-constrained CI
export SQRY_READ_BUFFER=4096     # 4 KB
export SQRY_PARSE_BUFFER=16384   # 16 KB
export SQRY_INDEX_BUFFER=262144  # 256 KB
```

### Memory-Mapped I/O

Files larger than `SQRY_MMAP_THRESHOLD` (default: 10 MB) use memory-mapped I/O instead of buffered reads. This improves performance for large index files by leveraging the OS page cache.

```bash
# Lower threshold for systems with plenty of RAM
export SQRY_MMAP_THRESHOLD=$((5 * 1024 * 1024))  # 5 MB

# Raise threshold if mmap causes issues (e.g., network filesystems)
export SQRY_MMAP_THRESHOLD=$((50 * 1024 * 1024))  # 50 MB
```

---

## Search Configuration

### Fuzzy Search Modes

sqry supports two fuzzy matching algorithms:

- **Jaccard** (default, `SQRY_FUZZY_USE_JACCARD=1`): Trigram-based Jaccard similarity. Faster for large symbol sets; best for "find something close to this name" queries.
- **Ratio** (`SQRY_FUZZY_USE_JACCARD=0`): Character-level edit distance ratio. More precise for short names; slower on large symbol sets.

```bash
# Use ratio mode for precision on small codebases
export SQRY_FUZZY_USE_JACCARD=0
sqry --fuzzy "patern" .
```

### Fallback Search

When semantic (AST) search returns too few results, sqry can fall back to text search (ripgrep). The fallback is enabled by default.

```bash
# Increase threshold: fall back only when semantic returns <5 results
export SQRY_MIN_SEMANTIC_RESULTS=5

# Disable fallback entirely (semantic-only)
export SQRY_FALLBACK_ENABLED=false
```

---

## Index Optimization

### Reducing Index Size

The `.sqry/graph/snapshot.sqry` index scales with file count and symbol density. To reduce index size:

```bash
# sqry respects .gitignore automatically
echo "node_modules/" >> .gitignore
echo "target/" >> .gitignore
echo "vendor/" >> .gitignore
echo "dist/" >> .gitignore

# Rebuild after adding exclusions
sqry index --force .
```

### CLI Tuning Flags

#### Index Command

The following flags are accepted by the CLI but are **not yet wired** into the unified graph build pipeline:

```bash
# Accepted but currently not applied:
sqry index --threads 4 .           # Thread count (build uses auto-detect)
sqry index --no-incremental .      # Disable incremental (no effect currently)
sqry index --cache-dir /tmp/cache .  # Cache dir override (no effect currently)
```

These flags exist for forward compatibility and may be activated in a future release.

#### Query Command

```bash
# Disable parallel query execution
sqry query --no-parallel "kind:function" .

# Use persistent session (keeps index hot for repeated queries)
sqry query --session "kind:function" .
```

### Index Build vs Update

```bash
# Initial build (creates the index; exits early if index already exists)
sqry index .

# Update existing index (rebuilds via unified pipeline)
sqry update .

# Force full rebuild from scratch
sqry index --force .
```

**Note**: `sqry index .` exits early if an index already exists. Use `sqry update .` to refresh an existing index, or `sqry index --force .` for a complete rebuild. The `update` command currently performs a full rebuild via the unified pipeline; true incremental (changed-files-only) updates are planned for a future release.

### Index Format

The unified graph snapshot uses postcard serialization with length-prefixed framing. The CLI accepts a `--no-compress` flag but it is not wired into the unified graph build pipeline — the flag is accepted for forward compatibility but currently has no effect.

### Index Validation

The `--validate` flag is a global option that controls validation when the index is loaded for queries. It does not run during the `sqry index` build itself:

```bash
# Query with validation warnings
sqry query --validate=warn "kind:function" .

# Strict validation mode
sqry query --validate=fail "kind:function" .
```

---

## Graph Analysis Performance

### Precomputed Analysis (Pass 5)

sqry precomputes three data structures for fast graph queries:

1. **SCCs** (Strongly Connected Components) — O(1) cycle membership checks
2. **Condensation DAG** — acyclic graph for efficient path traversal
3. **2-Hop Interval Labels** — fast reachability queries

These provide 10–1000x speedups over naive traversal:

| Operation | Without precomputation | With precomputation |
|-----------|----------------------|---------------------|
| Cycle detection | O(V+E) per query | O(1) lookup |
| Path finding | O(V+E) BFS | Pruned BFS via DAG |
| Unused code detection | O(\|entries\| x (V+E)) | O(\|entries\| x \|SCCs\| x \|L\|) |

### Avoiding Hangs on Large Graphs

The `sqry graph complexity` command can hang on very large codebases (167 MB+ indexes) due to exponential paths. If this happens:

```bash
# Kill the stuck process
kill -9 <pid>

# Use targeted queries instead of whole-graph analysis
sqry graph call-chain-depth "specific_function"

# Or limit scope to a specific module
sqry query "kind:function AND path:src/specific_module"
```

---

## SIMD Acceleration

sqry uses SIMD instructions for text search operations when available. The platform is detected at runtime — no configuration is required.

### Supported Platforms

| Platform | Instruction Set | Speedup |
|----------|----------------|---------|
| x86_64 | AVX2 (32-byte vectors) | 3–4x vs scalar |
| x86_64 | SSE4.2 (16-byte vectors, fallback) | 2–3x vs scalar |
| aarch64 | NEON | Comparable to SSE4.2 |
| Other | Scalar fallback | Baseline |

### Accelerated Operations

- **Substring search**: Boyer-Moore-Horspool with SIMD first-byte scan
- **Trigram extraction**: Bulk loading for fuzzy search candidate generation
- **ASCII case conversion**: Range-check vectorization

SIMD benefits are most visible for queries with needles longer than 8 bytes. Short queries (1–3 characters) are already fast with scalar code.

---

## Memory Management

### String Interning

sqry deduplicates symbol names via `StringInterner`, storing each unique string once with `Arc<str>`. This saves 10–50 MB on typical graph indexes.

The interner uses reference counting and a free list for slot reuse, so deleted symbols release memory for future allocations.

### Concurrency Model

- **MVCC snapshots**: Single-writer, multi-reader with epoch-based snapshots
- **Arc-wrapped CodeGraph**: O(1) snapshot creation for concurrent queries
- **Rayon**: Data-parallel file indexing
- **parking_lot**: High-performance locks (faster than `std::sync::Mutex`)

### OS Page Cache

The graph index is memory-mapped on first access. The first query after a reboot may be slow as the OS loads the file into the page cache. Subsequent queries reuse cached pages and are fast.

---

## Network Filesystem Detection

sqry can detect network-mounted filesystems during configuration initialization. Detection is available on all major platforms:

| Platform | Detection Method |
|----------|-----------------|
| Linux | `statfs` magic numbers (NFS, SMB, CIFS, AFS, CODA) |
| macOS | `statfs` `f_fstypename` field (nfs, smbfs, afpfs, webdav, ftp) |
| Windows | UNC path detection + `GetDriveTypeW` (DRIVE_REMOTE) |

### Recommendations for Network Filesystems

```bash
# Option 1: Copy to local disk for indexing
cp -r /mnt/network/repo /tmp/local-repo
cd /tmp/local-repo && sqry index .

# Option 2: Use watch mode to keep index warm
sqry watch --build .

# Option 3: Raise mmap threshold to avoid mmap over network
export SQRY_MMAP_THRESHOLD=$((100 * 1024 * 1024))  # 100 MB
```

---

## Benchmarking

sqry includes Criterion benchmarks for performance regression detection.

### Running Benchmarks

```bash
# Run all benchmarks
cargo bench --workspace

# Run specific benchmark
cargo bench -p sqry-core --bench query_benchmarks

# Available benchmarks
cargo bench -p sqry-core --bench ast_operations
cargo bench -p sqry-core --bench fuzzy_jaccard_benchmark
cargo bench -p sqry-core --bench hybrid_search_benchmark
cargo bench -p sqry-core --bench parallel_query_benchmarks
cargo bench -p sqry-core --bench query_e2e_profiling
cargo bench -p sqry-core --bench incremental_parsing
cargo bench -p sqry-core --bench plugin_loading_benchmark
```

### Profiling

```bash
# Run with allocation profiling (requires dhat-heap feature)
cargo test -p sqry-core --features dhat-heap

# Run with Rust backtraces
RUST_BACKTRACE=1 sqry index .
```

---

## Workload Profiles

### Small Codebase (<10K files)

Defaults work well. No tuning needed.

### Medium Codebase (10K–100K files)

```bash
# Increase cache for better hit rates
export SQRY_CACHE_MAX_BYTES=$((100 * 1024 * 1024))  # 100 MB

# Use .gitignore to exclude generated code
echo "*.generated.*" >> .gitignore
echo "vendor/" >> .gitignore
```

### Large Monorepo (100K+ files)

```bash
# Large cache with TinyLFU for hot-set protection
export SQRY_CACHE_MAX_BYTES=$((500 * 1024 * 1024))  # 500 MB
export SQRY_CACHE_POLICY=tiny_lfu

# Larger buffers for faster I/O
export SQRY_INDEX_BUFFER=$((4 * 1024 * 1024))  # 4 MB
export SQRY_PARSE_BUFFER=$((256 * 1024))        # 256 KB

# Exclude test fixtures and build artifacts
echo "test-fixtures/" >> .gitignore
echo "build/" >> .gitignore
echo "dist/" >> .gitignore
```

### CI/CD Pipelines

```bash
# Disable cache persistence (ephemeral environment)
export SQRY_CACHE_DISABLE_PERSIST=1

# Minimal buffers to reduce memory usage
export SQRY_READ_BUFFER=4096
export SQRY_PARSE_BUFFER=16384

# Use tmpfs for cache if available
export SQRY_CACHE_ROOT=/dev/shm/sqry-cache
```

---

## Getting Help

If performance issues persist after tuning:

1. Run `sqry cache stats` to check cache hit rates
2. Run `sqry graph stats` to check index size and composition
3. Use `RUST_BACKTRACE=1` for detailed error context
4. Open an issue with your workload profile, index stats, and the specific operation that is slow
