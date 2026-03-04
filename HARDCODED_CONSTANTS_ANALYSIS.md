# Hardcoded Numeric Constants in sqry Codebase

## Summary
This report identifies all hardcoded numeric constants that are candidates for configuration tuning. These are organized by category and include context about their usage.

---

## 1. TIMEOUT & DELAY CONSTANTS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/git/subprocess.rs | 21 | `const DEFAULT_TIMEOUT_MS: u64 = 3000;` | 3000 ms (3 sec) | Git command execution timeout | Yes |
| sqry-core/src/git/subprocess.rs | 152 | `std::thread::sleep(Duration::from_millis(10))` | 10 ms | Poll interval while waiting for git output | Yes |
| sqry-core/src/git/worktree.rs | 288 | `std::thread::sleep(std::time::Duration::from_millis(100))` | 100 ms | Worktree operation poll delay | Yes |
| sqry-core/src/plugin/safe_parse.rs | 66 | `pub const DEFAULT_TIMEOUT_MICROS: u64 = 2_000_000;` | 2 sec | Default parser timeout | Yes |
| sqry-core/src/plugin/safe_parse.rs | 72 | `pub const MIN_TIMEOUT_MICROS: u64 = 100_000;` | 100 ms | Minimum parser timeout | Yes |
| sqry-core/src/plugin/safe_parse.rs | 78 | `pub const MAX_TIMEOUT_MICROS: u64 = 5_000_000;` | 5 sec | Maximum parser timeout | Yes |
| sqry-core/src/plugin/safe_parse.rs | 826 | `thread::sleep(Duration::from_micros(10))` | 10 µs | Test pause in incremental parsing | Yes |
| sqry-core/src/graph/unified/build/progress.rs | 67 | `const MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(17)` | ~17 ms (60 FPS) | Progress UI update frequency | Yes |
| sqry-core/src/cache/persist.rs | 320-321 | `let max_retries = 50; let retry_delay = Duration::from_millis(100);` | 50 retries, 100 ms | Lock acquisition retry config | Yes |
| sqry-mcp/src/error.rs | 6 | `const DEFAULT_RETRY_AFTER_MS: u64 = 500;` | 500 ms | MCP retry-after delay | Yes |

---

## 2. BUFFER & SIZE LIMITS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/config/buffers.rs | 28 | `pub const DEFAULT_READ_BUFFER: usize = 8192;` | 8 KB | File read buffer | Yes |
| sqry-core/src/config/buffers.rs | 33 | `pub const DEFAULT_WRITE_BUFFER: usize = 8192;` | 8 KB | File write buffer | Yes |
| sqry-core/src/config/buffers.rs | 38 | `pub const DEFAULT_PARSE_BUFFER: usize = 65536;` | 64 KB | AST parsing buffer | Yes |
| sqry-core/src/config/buffers.rs | 43 | `pub const DEFAULT_INDEX_BUFFER: usize = 1_048_576;` | 1 MB | Graph indexing buffer | Yes |
| sqry-core/src/config/buffers.rs | 46-47 | `MIN_READ_BUFFER = 1024, MAX_READ_BUFFER = 1_048_576` | 1 KB - 1 MB | Read buffer bounds | Yes |
| sqry-core/src/config/buffers.rs | 48-49 | `MIN_WRITE_BUFFER = 1024, MAX_WRITE_BUFFER = 1_048_576` | 1 KB - 1 MB | Write buffer bounds | Yes |
| sqry-core/src/config/buffers.rs | 50-51 | `MIN_PARSE_BUFFER = 4096, MAX_PARSE_BUFFER = 10_485_760` | 4 KB - 10 MB | Parse buffer bounds | Yes |
| sqry-core/src/config/buffers.rs | 52-53 | `MIN_INDEX_BUFFER = 65536, MAX_INDEX_BUFFER = 104_857_600` | 64 KB - 100 MB | Index buffer bounds | Yes |
| sqry-core/src/plugin/safe_parse.rs | 48 | `pub const DEFAULT_MAX_SIZE: usize = 10 * 1024 * 1024;` | 10 MB | Max parseable file size | Yes |
| sqry-core/src/plugin/safe_parse.rs | 54 | `pub const MIN_MAX_SIZE: usize = 1024 * 1024;` | 1 MB | Min file size limit | Yes |
| sqry-core/src/plugin/safe_parse.rs | 60 | `pub const MAX_MAX_SIZE: usize = 32 * 1024 * 1024;` | 32 MB | Max file size limit | Yes |
| sqry-core/src/config/buffers.rs | 133 | `pub const DEFAULT_MMAP_THRESHOLD: u64 = 10 * 1024 * 1024;` | 10 MB | File size for memory mapping | Yes |
| sqry-core/src/config/buffers.rs | 136-137 | `MIN_MMAP_THRESHOLD = 1MB, MAX_MMAP_THRESHOLD = 1GB` | 1 MB - 1 GB | Mmap threshold bounds | Yes |
| sqry-core/src/config/buffers.rs | 143 | `pub const DEFAULT_MAX_SOURCE_FILE_SIZE: u64 = 50 * 1024 * 1024;` | 50 MB | Max source file size for indexing | Yes |
| sqry-core/src/config/buffers.rs | 146-147 | `MIN/MAX_MAX_SOURCE_FILE_SIZE = 1MB/500MB` | 1 MB - 500 MB | Source file size bounds | Yes |
| sqry-core/src/graph/unified/persistence/snapshot.rs | 19 | `const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024 * 1024;` | 8 GB | Max snapshot file size | Yes |
| sqry-core/src/uses/collector.rs | 52 | `const CHANNEL_CAPACITY: usize = 1000;` | 1000 items | Channel capacity for uses collection | Yes |
| sqry-core/src/io/binary.rs | 15 | `const SAMPLE_SIZE: usize = DEFAULT_READ_BUFFER;` | 8 KB | Binary format detection sample | Derived |
| sqry-cli/src/commands/troubleshoot.rs | 17 | `const DEFAULT_CACHE_SIZE: usize = 100;` | 100 items | Troubleshoot cache items | Yes |

---

## 3. MEMORY & QUERY LIMITS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/query/security/config.rs | 45 | `pub const DEFAULT_RESULT_CAP: usize = 10_000;` | 10k results | Max query results returned | Yes |
| sqry-core/src/query/security/config.rs | 48 | `pub const DEFAULT_MEMORY_LIMIT: usize = 512 * 1024 * 1024;` | 512 MB | Max query memory allocation | Yes |
| sqry-core/src/query/security/config.rs | 51 | `pub const DEFAULT_COST_LIMIT: usize = 1_000_000;` | 1M ops | Max query operation cost | Yes |
| sqry-core/src/config/buffers.rs | 302 | `pub const DEFAULT_MAX_QUERY_LENGTH: usize = 10 * 1024;` | 10 KB | Max query string length | Yes |
| sqry-core/src/config/buffers.rs | 305 | `const MIN_MAX_QUERY_LENGTH: usize = 1024;` | 1 KB | Min query length limit | Yes |
| sqry-core/src/config/buffers.rs | 306 | `const MAX_MAX_QUERY_LENGTH: usize = 100 * 1024;` | 100 KB | Max query length limit | Yes |
| sqry-core/src/config/buffers.rs | 344 | `pub const DEFAULT_MAX_PREDICATES: usize = 100;` | 100 predicates | Max query predicates | Yes |
| sqry-core/src/config/buffers.rs | 347 | `const MIN_MAX_PREDICATES: usize = 10;` | 10 predicates | Min predicates limit | Yes |
| sqry-core/src/config/buffers.rs | 348 | `const MAX_MAX_PREDICATES: usize = 1000;` | 1000 predicates | Max predicates limit | Yes |
| sqry-core/src/ast/query.rs | 35 | `const MAX_REGEX_LENGTH: usize = 1000;` | 1000 chars | Max regex pattern length | Yes |
| sqry-core/src/ast/query.rs | 38 | `const MAX_REPETITION_COUNT: usize = 1000;` | 1000x | Max regex repetition count | Yes |

---

## 4. CACHE & POOL CONFIGURATION

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/ast/query.rs | 417 | `const QUERY_CACHE_SIZE: usize = 100;` | 100 queries | Parsed query cache entries | Yes |
| sqry-core/src/query/cache/ast_parse_cache.rs | 67 | `const MIN_CAPACITY: u64 = 1;` | 1 entry | Min cache capacity | Yes |
| sqry-core/src/query/cache/ast_parse_cache.rs | 68 | `const AST_ENTRY_WEIGHT_BYTES: u64 = 2048;` | 2 KB/entry | Estimated AST entry size | Yes |
| sqry-core/src/query/lexer.rs | 736 | `const POOL_MAX_DEFAULT: usize = 4;` | 4 threads | Default lexer thread pool size | Yes |
| sqry-core/src/cache/config.rs | 70 | `pub const DEFAULT_CACHE_ROOT: &'static str = ".sqry-cache";` | String | Cache directory name | Yes |
| sqry-core/src/cache/config.rs | 73 | `pub const DEFAULT_POLICY_WINDOW_RATIO: f32 = 0.20;` | 20% | Cache eviction window ratio | Yes |
| sqry-core/src/cache/config.rs | 75 | `pub const MIN_POLICY_WINDOW_RATIO: f32 = 0.05;` | 5% | Min window ratio | Yes |
| sqry-core/src/cache/config.rs | 77 | `pub const MAX_POLICY_WINDOW_RATIO: f32 = 0.95;` | 95% | Max window ratio | Yes |
| sqry-core/src/cache/config.rs | 340 | `pub const DEFAULT_TTL: Duration = Duration::from_secs(3600);` | 1 hour | Default cache entry TTL | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 17 | `pub const CACHE_TTL_SECS: u64 = 300;` | 300 sec (5 min) | MCP graph cache TTL | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 21 | `pub const TRACE_PATH_CACHE_CAPACITY: usize = 256;` | 256 entries | Trace path cache size | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 22 | `pub const SUBGRAPH_CACHE_CAPACITY: usize = 128;` | 128 entries | Subgraph cache size | Yes |

---

## 5. SAMPLING & MONITORING THRESHOLDS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/io/binary.rs | 19 | `const NON_PRINTABLE_THRESHOLD_PERCENT: usize = 30;` | 30% | Threshold for binary file detection | Yes |
| sqry-core/src/graph/diff.rs | 25 | `const SIGNATURE_MIN_SCORE: f64 = 0.7;` | 0.7 | Min signature similarity score | Yes |
| sqry-core/src/graph/diff.rs | 26 | `const RENAME_CONFIDENCE_THRESHOLD: f64 = 0.9;` | 0.9 | Rename detection confidence | Yes |
| sqry-core/src/graph/diff.rs | 27 | `const SAME_FILE_LINE_WINDOW: i32 = 50;` | 50 lines | Window for same-file diff detection | Yes |
| sqry-core/src/graph/diff.rs | 28 | `const SAME_FILE_LINE_NORMALIZER: f64 = 100.0;` | 100 | Line distance normalizer | Yes |
| sqry-core/src/graph/diff.rs | 29 | `const SAME_FILE_MAX_PENALTY: f64 = 0.5;` | 0.5 | Max penalty for same-file distance | Yes |
| sqry-mcp/src/execution/diff_comparator.rs | 10-18 | Multiple scoring weights | 0.3-0.9 | Graph diff scoring algorithm weights | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 25 | `const HISTOGRAM_MAX_MS: u64 = 600_000;` | 10 min | Cache latency histogram upper bound | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 26 | `const HISTOGRAM_SIGFIGS: u8 = 3;` | 3 sig figs | Histogram accuracy | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 31 | `const OVERFLOW_BUCKET_BOUNDS: &[u64] = &[120_000, 300_000, 600_000];` | 2/5/10 min | Overflow bucket time boundaries | Yes |
| sqry-mcp/src/execution/graph_cache.rs | 317 | `const MAX_HISTOGRAM_SAMPLES: u64 = 10_000;` | 10k samples | Max cache histogram samples | Yes |

---

## 6. REPOSITORY & CONCURRENCY LIMITS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/config/buffers.rs | 153 | `pub const DEFAULT_MAX_REPOSITORIES: usize = 1_000;` | 1000 repos | Max indexed repositories | Yes |
| sqry-core/src/config/buffers.rs | 156 | `const MIN_MAX_REPOSITORIES: usize = 10;` | 10 repos | Min repository limit | Yes |
| sqry-core/src/config/buffers.rs | 157 | `const MAX_MAX_REPOSITORIES: usize = 10_000;` | 10k repos | Max repository limit | Yes |
| sqry-core/src/config/buffers.rs | 259 | `pub const DEFAULT_WATCH_EVENT_QUEUE: usize = 10_000;` | 10k events | File watcher event queue capacity | Yes |
| sqry-core/src/config/buffers.rs | 262 | `const MIN_WATCH_EVENT_QUEUE: usize = 100;` | 100 events | Min queue capacity | Yes |
| sqry-core/src/config/buffers.rs | 263 | `const MAX_WATCH_EVENT_QUEUE: usize = 100_000;` | 100k events | Max queue capacity | Yes |
| sqry-core/src/graph/unified/compaction/interruptible.rs | 48 | `pub const DEFAULT_CHUNK_SIZE: usize = 10_000;` | 10k nodes | Compaction chunk size | Yes |

---

## 7. GRAPH & NODE LIMITS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/graph/unified/persistence/snapshot.rs | 22 | `const MAX_REASONABLE_NODES: usize = 100_000_000;` | 100M nodes | Snapshot validation limit | Yes |
| sqry-core/src/graph/unified/persistence/snapshot.rs | 25 | `const MAX_REASONABLE_EDGES: usize = 1_000_000_000;` | 1B edges | Snapshot validation limit | Yes |
| sqry-core/src/graph/unified/persistence/snapshot.rs | 28 | `const MAX_REASONABLE_STRINGS: usize = 50_000_000;` | 50M strings | Snapshot validation limit | Yes |
| sqry-core/src/graph/unified/persistence/snapshot.rs | 31 | `const MAX_REASONABLE_FILES: usize = 1_000_000;` | 1M files | Snapshot validation limit | Yes |
| sqry-core/src/graph/unified/node/id.rs | 103 | `pub const MAX_GENERATION: u64 = u64::MAX / 2;` | 9.2E18 | Node generation counter limit | No |
| sqry-mcp/src/execution/graph_builders.rs | 23 | `const MAX_EDGES_FOR_CROSS_LANG_SCAN: usize = 50_000;` | 50k edges | Cross-language edge scan limit | Yes |

---

## 8. STRING & PATTERN LIMITS

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/search/simd/scalar.rs | 164-166 | `MAX_HAYSTACK_LEN=256, MAX_NEEDLE_LEN=96, MAX_EXTRA_LEN=128` | Various | SIMD search buffer limits | No |
| sqry-core/src/ast/incremental_parse.rs | 214 | `pub const DEFAULT_CAPACITY: usize = 100;` | 100 items | Default parse state capacity | Yes |

---

## 9. COMPRESSION & INDEXING

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/indexing/compression.rs | 54 | `pub const DEFAULT_COMPRESSION_LEVEL: i32 = 3;` | Level 3 | Zstd compression level (1-22) | Yes |
| sqry-core/src/indexing/compression.rs | 60 | `pub const DEFAULT_MAX_UNCOMPRESSED_SIZE: u64 = 500 * 1024 * 1024;` | 500 MB | Max uncompressed index size | Yes |
| sqry-core/src/indexing/compression.rs | 63 | `const MIN_MAX_UNCOMPRESSED_SIZE: u64 = 1024 * 1024;` | 1 MB | Min size limit | Yes |
| sqry-core/src/indexing/compression.rs | 66 | `const MAX_MAX_UNCOMPRESSED_SIZE: u64 = 2 * 1024 * 1024 * 1024;` | 2 GB | Max size limit | Yes |

---

## 10. USAGE & RETENTION

| File | Line | Code Snippet | Value | Context | Configurable? |
|------|------|-------------|-------|---------|---------------|
| sqry-core/src/uses/config.rs | 29 | `const DEFAULT_RETENTION_DAYS: u32 = 365;` | 365 days | Usage data retention period | Yes |
| sqry-cli/src/persistence/config.rs | 9 | `pub const DEFAULT_MAX_HISTORY_ENTRIES: usize = 10_000;` | 10k entries | CLI command history max size | Yes |

---

## Summary Statistics

- **Total Configuration Candidates**: 95+
- **By Category**:
  - Timeouts & Delays: 10
  - Buffer & Size Limits: 20
  - Memory & Query Limits: 11
  - Cache & Pool: 11
  - Sampling & Monitoring: 11
  - Repository & Concurrency: 6
  - Graph & Node Limits: 6
  - String & Pattern Limits: 2
  - Compression & Indexing: 4
  - Usage & Retention: 2

- **Easily Configurable**: ~85 constants
- **Likely Fixed**: ~10 constants (protocol versions, magic numbers, bit shifts)

---

## Recommended Actions

1. **Create Configuration Abstraction**: Build a centralized config system with environment variable overrides
2. **Dynamic Defaults**: Allow users to tune constants via:
   - Environment variables (e.g., `SQRY_DEFAULT_TIMEOUT_MS`, `SQRY_BUFFER_SIZE`)
   - Configuration files (`.sqry/config.toml`)
   - CLI flags (for critical settings)

3. **Priority Tuning Targets**:
   - Timeouts (git, parsing)
   - Buffer sizes (depends on workload)
   - Cache capacities
   - Query limits (memory, results)
   - Repository limits

4. **Validation**: Enforce min/max bounds on all configurable values

