# Configuration Tuning Guide for sqry

**Quick Reference**: See `HARDCODED_CONSTANTS_ANALYSIS.md` for detailed analysis of 95+ configuration candidates.

## Performance Tuning Matrix

### 1. Fast Indexing (Small/Medium Projects)
```
SQRY_READ_BUFFER=16384              # 16 KB (larger reads)
SQRY_PARSE_BUFFER=131072            # 128 KB (faster AST parsing)
SQRY_INDEX_BUFFER=2097152           # 2 MB (bigger batches)
SQRY_PARSER_TIMEOUT_MS=3000         # 3 sec (more time)
SQRY_MAX_REPOSITORIES=500            # Adjust if needed
```

### 2. Memory-Constrained (Embedded/CI)
```
SQRY_READ_BUFFER=4096               # 4 KB (minimal)
SQRY_PARSE_BUFFER=32768             # 32 KB (minimal)
SQRY_INDEX_BUFFER=262144            # 256 KB (minimal)
SQRY_CACHE_TTL=300                  # 5 min (less memory)
SQRY_MAX_QUERY_RESULTS=5000          # Reduce output
```

### 3. Large Codebase (Enterprise)
```
SQRY_MMAP_THRESHOLD=5242880         # 5 MB (more mmap)
SQRY_MAX_SOURCE_FILE_SIZE=104857600 # 100 MB (larger files)
SQRY_MAX_REPOSITORIES=5000           # More repos
SQRY_WATCH_QUEUE_SIZE=50000          # Bigger queue
SQRY_SNAPSHOT_MAX_BYTES=17179869184  # 16 GB max
```

### 4. Aggressive Timeout Tuning
```
SQRY_GIT_TIMEOUT_MS=5000            # 5 sec (slow network)
SQRY_PARSER_TIMEOUT_MS=5000         # 5 sec (slow parsing)
SQRY_PARSER_TIMEOUT_MICROS=5000000  # 5 sec (same)
SQRY_RETRY_ATTEMPTS=100              # More retries
SQRY_RETRY_DELAY_MS=200              # Longer delay
```

## Environment Variables (Implemented)

### Core Buffers
- `SQRY_READ_BUFFER` - File read buffer size (bytes)
- `SQRY_WRITE_BUFFER` - File write buffer size (bytes)
- `SQRY_PARSE_BUFFER` - AST parsing buffer (bytes)
- `SQRY_INDEX_BUFFER` - Graph indexing buffer (bytes)

### Parser Configuration
- `SQRY_PARSER_TIMEOUT_MICROS` - Parser timeout (microseconds)
- `SQRY_MAX_FILE_SIZE` - Max parseable file (bytes)

### Git Operations
- `SQRY_GIT_TIMEOUT_MS` - Git command timeout (milliseconds)
- `SQRY_GIT_POLL_MS` - Git poll interval (milliseconds)

### Caching
- `SQRY_CACHE_TTL_SECS` - Cache entry TTL (seconds)
- `SQRY_QUERY_CACHE_SIZE` - Parsed queries cached
- `SQRY_CACHE_WINDOW_RATIO` - Eviction window ratio

### Query Limits
- `SQRY_MAX_QUERY_RESULTS` - Max returned results
- `SQRY_MAX_QUERY_LENGTH` - Max query string (bytes)
- `SQRY_MAX_PREDICATES` - Max predicates per query
- `SQRY_QUERY_MEMORY_LIMIT` - Max query memory (bytes)
- `SQRY_QUERY_COST_LIMIT` - Max operation cost

### Repository Management
- `SQRY_MAX_REPOSITORIES` - Max indexed repos
- `SQRY_WATCH_QUEUE_SIZE` - File watcher queue

### Graph Limits
- `SQRY_MAX_SNAPSHOT_BYTES` - Max snapshot size (bytes)
- `SQRY_MMAP_THRESHOLD` - Mmap file size (bytes)
- `SQRY_MAX_SOURCE_FILE_SIZE` - Max source file (bytes)

## Common Bottlenecks & Solutions

### Bottleneck: Slow Parsing
**Symptoms**: `parse timed out` errors
**Solutions**:
1. Increase `SQRY_PARSER_TIMEOUT_MICROS` (2s -> 5s)
2. Increase `SQRY_PARSE_BUFFER` (64KB -> 256KB)
3. Check file size: Increase `SQRY_MAX_FILE_SIZE` if needed

### Bottleneck: Memory Usage
**Symptoms**: OOM killer, swap thrashing
**Solutions**:
1. Reduce `SQRY_INDEX_BUFFER` (1MB -> 256KB)
2. Reduce `SQRY_QUERY_MEMORY_LIMIT` (512MB -> 256MB)
3. Reduce `SQRY_MAX_QUERY_RESULTS` (10K -> 1K)
4. Reduce `SQRY_CACHE_WINDOW_RATIO` (0.2 -> 0.1)

### Bottleneck: Network I/O (Git)
**Symptoms**: Git commands timing out
**Solutions**:
1. Increase `SQRY_GIT_TIMEOUT_MS` (3s -> 10s)
2. Increase `SQRY_GIT_POLL_MS` (10ms -> 50ms)
3. Check network: Consider local mirrors

### Bottleneck: Many Small Files
**Symptoms**: Slow indexing, low CPU
**Solutions**:
1. Increase thread pool: `SQRY_LEXER_POOL_MAX` (4 -> 8+)
2. Increase `SQRY_PARSE_BUFFER` for better batching
3. Increase `SQRY_WATCH_QUEUE_SIZE` for file watcher

### Bottleneck: Query Performance
**Symptoms**: Slow graph queries, high latency
**Solutions**:
1. Increase `SQRY_QUERY_CACHE_SIZE` (100 -> 500)
2. Increase `SQRY_QUERY_MEMORY_LIMIT` (512MB -> 2GB)
3. Increase `SQRY_MAX_QUERY_RESULTS` if needed
4. Reduce `SQRY_MAX_PREDICATES` for simplification

## Validation Rules

All configurable constants enforce these bounds:

| Parameter | Min | Default | Max |
|-----------|-----|---------|-----|
| READ_BUFFER | 1 KB | 8 KB | 1 MB |
| WRITE_BUFFER | 1 KB | 8 KB | 1 MB |
| PARSE_BUFFER | 4 KB | 64 KB | 10 MB |
| INDEX_BUFFER | 64 KB | 1 MB | 100 MB |
| PARSER_TIMEOUT | 100 ms | 2 sec | 5 sec |
| FILE_SIZE | 1 MB | 10 MB | 32 MB |
| MMAP_THRESHOLD | 1 MB | 10 MB | 1 GB |
| SOURCE_FILE_SIZE | 1 MB | 50 MB | 500 MB |
| MAX_REPOSITORIES | 10 | 1000 | 10K |
| WATCH_QUEUE | 100 | 10K | 100K |
| QUERY_RESULTS | - | 10K | - |
| QUERY_MEMORY | - | 512 MB | - |
| QUERY_COST | - | 1M ops | - |
| MAX_PREDICATES | 10 | 100 | 1000 |

## Profiling Commands

```bash
# Monitor buffer usage
RUST_LOG=sqry=debug sqry index 2>&1 | grep -i "buffer\|cache"

# Check git timeout issues
RUST_LOG=sqry=trace sqry index 2>&1 | grep -i "timeout\|git"

# Monitor memory usage
/usr/bin/time -v sqry index

# Profile with perf
perf record -g sqry index
perf report
```

## Configuration File Support (Future)

Planned `.sqry/config.toml`:
```toml
[buffers]
read_size = 8192
write_size = 8192
parse_size = 65536
index_size = 1048576

[timeouts]
git_ms = 3000
parser_ms = 2000
parser_poll_ms = 10

[caching]
ttl_secs = 3600
query_cache_size = 100

[limits]
max_query_results = 10000
max_query_memory = 536870912
max_predicates = 100
```

## References
- Detailed analysis: `HARDCODED_CONSTANTS_ANALYSIS.md`
- Configuration source: `sqry-core/src/config/buffers.rs`
- Parser config: `sqry-core/src/plugin/safe_parse.rs`
- Query security: `sqry-core/src/query/security/config.rs`
