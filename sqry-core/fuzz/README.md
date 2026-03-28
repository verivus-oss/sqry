# Fuzz Testing Infrastructure

**Part of**: P2-37 Phase 1 (RR-11: Defense-in-Depth Testing)
**Purpose**: Coverage-guided fuzzing to discover panics, assertion failures, and memory safety bugs in the query parser and all language plugins

## Overview

This directory contains comprehensive fuzz testing infrastructure using [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz), which leverages libFuzzer for coverage-guided testing.

**Current Status**:
- ✅ **37 Total Fuzz Targets**: 1 query parser + 35 language plugins + 1 additional
- ✅ **Complete Language Coverage**: All 35 supported languages (C, C++, C#, CSS, Dart, Elixir, Go, Groovy, Haskell, HTML, Java, JavaScript, Kotlin, Lua, Oracle PL/SQL, Perl, PHP, Pulumi, Puppet, Python, R, Ruby, Rust, Salesforce Apex, SAP ABAP, Scala, ServiceNow Xanadu, Shell, SQL, Svelte, Swift, Terraform, TypeScript, Vue, Zig)
- ✅ **Query Parser Coverage**: 8,860+ code edges, zero crashes (validates Phase 0 Fix #3 error handling)

## Fuzz Targets

### Query Parser

**Target**: `query_parser`
**File**: `fuzz_targets/query_parser.rs`
**Purpose**: Fuzz the sqry query parser to discover panics in query parsing logic
**Corpus**: 46 seed files covering valid queries, invalid queries, and edge cases

### Language Plugins (34 Total)

All 34 language plugin fuzz targets follow the same pattern:
1. Parse AST using `plugin.parse_ast(data)`
2. Extract symbols using `plugin.extract_symbols_from_tree()`
3. Extract imports using `plugin.extract_imports()`
4. Extract exports using `plugin.extract_exports()`
5. Extract calls using `plugin.extract_calls()`

Each plugin has 3 seed files (empty, minimal, comprehensive) covering language-specific syntax features.

| Target | Language | File Extension | Corpus Seeds |
|--------|----------|----------------|--------------|
| `c_plugin` | C | `.c` | 3 files |
| `cpp_plugin` | C++ | `.cpp` | 3 files |
| `csharp_plugin` | C# | `.cs` | 3 files |
| `css_plugin` | CSS | `.css` | 3 files |
| `dart_plugin` | Dart | `.dart` | 3 files |
| `elixir_plugin` | Elixir | `.ex` | 3 files |
| `go_plugin` | Go | `.go` | 3 files |
| `groovy_plugin` | Groovy | `.groovy` | 3 files |
| `haskell_plugin` | Haskell | `.hs` | 3 files |
| `html_plugin` | HTML | `.html` | 3 files |
| `java_plugin` | Java | `.java` | 3 files |
| `javascript_plugin` | JavaScript | `.js` | 3 files |
| `kotlin_plugin` | Kotlin | `.kt` | 3 files |
| `lua_plugin` | Lua | `.lua` | 3 files |
| `oracle_plsql_plugin` | Oracle PL/SQL | `.sql` | 3 files |
| `perl_plugin` | Perl | `.pl` | 3 files |
| `php_plugin` | PHP | `.php` | 3 files |
| `puppet_plugin` | Puppet | `.pp` | 3 files |
| `python_plugin` | Python | `.py` | 3 files |
| `r_plugin` | R | `.r` | 3 files |
| `ruby_plugin` | Ruby | `.rb` | 3 files |
| `rust_plugin` | Rust | `.rs` | 3 files |
| `salesforce_apex_plugin` | Salesforce Apex | `.cls` | 3 files |
| `sap_abap_plugin` | SAP ABAP | `.abap` | 3 files |
| `scala_plugin` | Scala | `.scala` | 3 files |
| `servicenow_xanadu_plugin` | ServiceNow Xanadu | `.js` | 3 files |
| `shell_plugin` | Shell | `.sh` | 3 files |
| `sql_plugin` | SQL | `.sql` | 3 files |
| `svelte_plugin` | Svelte | `.svelte` | 3 files |
| `swift_plugin` | Swift | `.swift` | 3 files |
| `terraform_plugin` | Terraform | `.tf` | 3 files |
| `typescript_plugin` | TypeScript | `.ts` | 3 files |
| `vue_plugin` | Vue | `.vue` | 3 files |
| `zig_plugin` | Zig | `.zig` | 3 files |

**List all targets**:

```bash
cd sqry-core
cargo +nightly fuzz list
```

## Prerequisites

```bash
# Install cargo-fuzz (one-time setup)
cargo install cargo-fuzz

# Install Rust nightly toolchain (required for fuzzing)
rustup toolchain install nightly
```

## Quick Start

### Generate Seed Corpus

**Query Parser**: The seed corpus contains 46 high-quality test cases derived from parser unit tests:

```bash
cd sqry-core
./scripts/generate_fuzz_corpus.sh
```

This creates `fuzz/corpus/query_parser/seeds/` with test cases covering:
- Valid queries (operators, precedence, scope filters)
- Invalid queries (error cases)
- Edge cases (whitespace-only, deeply nested, max length)

**Language Plugins**: Each plugin has 3 seed files (empty, minimal, comprehensive) automatically created in `fuzz/corpus/<plugin_name>/seeds/`. No separate script needed.

### Run Quick Smoke Test (1 minute)

Fast test without sanitizers for development.

**Query Parser**:

```bash
cd sqry-core
cargo +nightly fuzz run query_parser \
  --sanitizer=none \
  -- \
  -max_total_time=60 \
  -max_len=2048
```

**Language Plugin** (example: Python):

```bash
cd sqry-core
cargo +nightly fuzz run python_plugin \
  --sanitizer=none \
  -- \
  -max_total_time=60 \
  -max_len=8192
```

**Expected output**: `Done N runs in 60 second(s)` with exit code 0 (no crashes)

**Note**: Language plugins use `-max_len=8192` (vs 2048 for query parser) since source code files are typically larger than queries.

### Run Baseline Test (10 minutes)

Standard test with AddressSanitizer for bug detection.

**Query Parser**:

```bash
cd sqry-core
cargo +nightly fuzz run query_parser \
  -- \
  -max_total_time=600 \
  -max_len=2048
```

**Language Plugin** (example: Rust):

```bash
cd sqry-core
cargo +nightly fuzz run rust_plugin \
  -- \
  -max_total_time=600 \
  -max_len=8192
```

**Metrics to observe**:
- `cov: XXXX` - Code coverage (edges explored)
- `ft: YYYY` - Features discovered
- `corp: ZZZZ` - Corpus size (test cases)
- `exec/s: WWW` - Execution throughput

### Run Extended Test (2-4 hours)

Deep testing for thorough validation.

**Query Parser**:

```bash
cd sqry-core
cargo +nightly fuzz run query_parser \
  -- \
  -max_total_time=14400 \
  -max_len=2048
```

**Language Plugin** (example: Java):

```bash
cd sqry-core
cargo +nightly fuzz run java_plugin \
  -- \
  -max_total_time=14400 \
  -max_len=8192
```

## Corpus Management

### Directory Structure

**Query Parser** (46 seed files):

```
fuzz/corpus/query_parser/
├── seeds/          # 46 versioned seed files (committed to git)
│   ├── 001_simple_condition
│   ├── 002_and_expression
│   └── ...
└── generated/      # Fuzzer-discovered cases (git-ignored)
    ├── 0a3f2b1c...
    └── ...
```

**Language Plugins** (3 seed files each × 34 = 102 total):

```
fuzz/corpus/python_plugin/
├── seeds/          # 3 versioned seed files (committed to git)
│   ├── empty.py
│   ├── minimal.py
│   └── comprehensive.py
└── generated/      # Fuzzer-discovered cases (git-ignored)
    ├── 1f2e3d4c...
    └── ...
```

**Version Control**:
- `seeds/` - Committed to git, manually curated
- `generated/` - Ignored via `.gitignore`, auto-generated by fuzzer

### Corpus Minimization

After extended fuzzing runs, minimize the corpus to reduce redundancy while preserving coverage:

```bash
cd sqry-core
cargo +nightly fuzz cmin query_parser -- -max_len=2048
```

**What it does**:
- Analyzes all test cases in the corpus
- Keeps only the minimal set that achieves full coverage
- Removes redundant/duplicate test cases

**Example**: 3,921 files → 2,676 files (31% reduction, same coverage)

### Corpus Refresh

Regenerate seeds after parser changes or test updates:

```bash
cd sqry-core

# 1. Backup existing generated corpus (optional)
cp -r fuzz/corpus/query_parser/generated fuzz/corpus/query_parser/generated.backup

# 2. Regenerate seeds
./scripts/generate_fuzz_corpus.sh

# 3. Re-minimize corpus
cargo +nightly fuzz cmin query_parser -- -max_len=2048
```

## Crash Triage Workflow

If fuzzing discovers a crash, follow this 5-step workflow:

### Step 1: Minimize Crash Input

Reduce the crash input to the smallest possible test case:

```bash
cd sqry-core

# Find crash file in artifacts directory
ls fuzz/artifacts/query_parser/

# Minimize it (example: crash-abc123)
cargo +nightly fuzz tmin query_parser \
  fuzz/artifacts/query_parser/crash-abc123 \
  -- \
  -max_len=2048
```

**Output**: Minimized crash input overwrites the original artifact

### Step 2: Reproduce Crash

Verify the crash is reproducible:

```bash
cd sqry-core

# Run fuzz target with the minimized crash input
cargo +nightly fuzz run query_parser \
  fuzz/artifacts/query_parser/crash-abc123
```

**Expected**: Crash reproduces consistently

### Step 3: Dual Promotion

Add the crash to both the corpus (for future fuzzing) AND create a regression test:

**3a. Add to Corpus**:

```bash
# Copy to seeds for version control
cp fuzz/artifacts/query_parser/crash-abc123 \
   fuzz/corpus/query_parser/seeds/999_crash_YYYYMMDD_description
```

**3b. Create Regression Test**:

Add to `sqry-core/src/query/parser_new.rs` unit tests:

```rust
#[test]
fn test_crash_YYYYMMDD_description() {
    // Regression test for crash discovered by fuzzing
    let input = "..."; // The minimized crash input
    let result = Parser::parse_query(input);

    // Test should NOT panic - either parse successfully or return error
    match result {
        Ok(_) => { /* Valid parse */ }
        Err(e) => {
            // Ensure error is graceful, not a panic
            assert!(e.to_string().contains("expected keyword"));
        }
    }
}
```

### Step 4: Fix Parser Bug

Investigate the crash and fix the root cause in `parser_new.rs`. Common patterns:
- Missing error handling (`.expect()` → `.map_err()?`)
- Unchecked array indexing
- Integer overflow
- Unsafe code bugs

### Step 5: Verify Fix

Run verification in BOTH sanitizer modes:

**5a. Fast verification (no sanitizers)**:

```bash
cargo +nightly fuzz run query_parser \
  --sanitizer=none \
  fuzz/artifacts/query_parser/crash-abc123
```

**5b. Thorough verification (with sanitizers)**:

```bash
cargo +nightly fuzz run query_parser \
  fuzz/artifacts/query_parser/crash-abc123
```

**5c. Run regression test**:

```bash
cargo test test_crash_YYYYMMDD_description
```

**All three must pass** before merging the fix.

## Parallel Fuzzing

For faster coverage, run multiple fuzzer instances:

```bash
cd sqry-core

# Run 4 parallel fuzzer jobs
cargo +nightly fuzz run query_parser \
  -- \
  -max_total_time=3600 \
  -max_len=2048 \
  -jobs=4
```

**Note**: Requires sufficient CPU cores and memory (~500MB per job)

## CI Integration

### PR Smoke Test

Runs automatically on pull requests affecting parser code:
- **Duration**: 1 minute
- **Sanitizers**: Disabled (speed)
- **Purpose**: Quick validation

### Nightly Fuzz Test

Runs daily at 2 AM UTC via GitHub Actions:
- **Duration**: 10 minutes
- **Sanitizers**: Enabled (AddressSanitizer)
- **Toolchain**: Pinned to `nightly-2025-11-22`
- **Purpose**: Deep validation

### Manual Trigger

Run custom fuzz tests via GitHub Actions UI:
- Navigate to Actions → Query Parser Fuzz Testing
- Click "Run workflow"
- Configure duration and sanitizers

## Maintenance Tasks

### Weekly

- Review nightly fuzz test results for crashes
- Minimize corpus if it exceeds 5,000 files

### After Parser Changes

- Regenerate seed corpus (`./scripts/generate_fuzz_corpus.sh`)
- Run 10-minute baseline test
- Update regression tests if behavior changes

### Before Release

- Run extended 4-hour fuzz test
- Verify zero crashes
- Minimize corpus for optimal CI performance

## Troubleshooting

### "workspace" Error

**Error**: `current package believes it's in a workspace when it's not`

**Fix**: Ensure `fuzz/Cargo.toml` has `[workspace]` table and root workspace excludes `sqry-core/fuzz`

### Nightly Toolchain Issues

**Error**: `the option Z is only accepted on the nightly compiler`

**Fix**: Ensure using `cargo +nightly fuzz` or `cargo +nightly-2025-11-22 fuzz`

### Out of Memory

**Symptom**: Fuzzer killed by OOM

**Fix**: Reduce parallel jobs or use `-rss_limit_mb=1024` flag

### Low Coverage

**Symptom**: Coverage plateaus quickly

**Fix**:
1. Review seed corpus quality
2. Add edge cases to `generate_fuzz_corpus.sh`
3. Run longer (4+ hours)

## Technical Details

### Fuzz Targets

#### Query Parser

**File**: `fuzz/fuzz_targets/query_parser.rs`

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_core::query::parser_new::Parser;

fuzz_target!(|data: &[u8]| {
    if let Ok(query_str) = std::str::from_utf8(data) {
        let _ = Parser::parse_query(query_str);
    }
});
```

**Design**:
- UTF-8 validation gate prevents invalid input
- Discards parse results (only testing for panics)
- Relies on Phase 0 Fix #3 error handling

#### Language Plugins

**File**: `fuzz/fuzz_targets/{plugin_name}.rs` (example: `python_plugin.rs`)

```rust
#![no_main]
use libfuzzer_sys::fuzz_target;
use sqry_core::plugin::LanguagePlugin;
use sqry_lang_python::PythonPlugin;
use std::path::Path;

fuzz_target!(|data: &[u8]| {
    let plugin = PythonPlugin;
    let dummy_path = Path::new("fuzz.py");

    if let Ok(tree) = plugin.parse_ast(data) {
        let _ = plugin.extract_symbols_from_tree(&tree, data, dummy_path);
        let _ = plugin.extract_imports(&tree, data, dummy_path);
        let _ = plugin.extract_exports(&tree, data, dummy_path);
        let _ = plugin.extract_calls(&tree, data, dummy_path);
    }
});
```

**Design**:
- Fuzzes tree-sitter AST parsing for language-specific syntax
- Exercises symbol extraction (functions, classes, variables)
- Exercises relation extraction (imports, exports, calls)
- Tests language-specific features (decorators, async/await, macros, traits, etc.)
- All 34 plugins follow the same pattern with language-specific instantiation

### Sanitizers

**AddressSanitizer** (default):
- Detects: Use-after-free, buffer overflows, memory leaks
- Overhead: ~2x slowdown, ~3x memory usage
- Recommended for nightly/scheduled runs

**No Sanitizer** (`--sanitizer=none`):
- Detects: Panics only
- Overhead: Minimal
- Recommended for PR smoke tests

### libFuzzer Flags

Commonly used flags (passed after `--`):

- `-max_total_time=N` - Stop after N seconds
- `-max_len=N` - Maximum input size (bytes)
- `-jobs=N` - Parallel fuzzer instances
- `-rss_limit_mb=N` - Memory limit per job
- `-timeout=N` - Timeout for single test case
- `-dict=FILE` - Dictionary for mutations

**Example**:

```bash
cargo +nightly fuzz run query_parser -- \
  -max_total_time=600 \
  -max_len=2048 \
  -timeout=10 \
  -rss_limit_mb=2048
```

## References

- [cargo-fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz.html)
- [libFuzzer Documentation](https://llvm.org/docs/LibFuzzer.html)
- [P2-37 Unwrap Audit](../../docs/development/p2-37-unwrap-audit/)
- [Phase 1 Fix #4 Implementation Plan](../../docs/development/p2-37-unwrap-audit/phase1_fix4_parser_fuzzing_PLAN.md)

## Support

For issues or questions about fuzz testing:
1. Check this README first
2. Review [cargo-fuzz documentation](https://rust-fuzz.github.io/book/cargo-fuzz.html)
3. Consult Phase 1 Fix #4 implementation plan
4. Open GitHub issue with `fuzzing` label
