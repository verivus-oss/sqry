# OOM Vulnerability Test Fixtures

These artifacts are binary fuzzing outputs that trigger OOM vulnerabilities in tree-sitter parsers. They are checked into git (unlike `sqry-core/fuzz/artifacts/` which is gitignored) to ensure CI can run regression tests.

## Artifacts

### groovy/oom-4fa9af91ed04930f510f370b83906b6e9cb7d396
- **Bug**: BUG-2025-001
- **Size**: 103 bytes
- **Impact**: Caused 2GB+ memory consumption (~20 million x amplification)
- **Parse time (unmitigated)**: ~56 seconds before OOM
- **Root cause**: Pathological input triggers exponential backtracking in tree-sitter error recovery

### svelte/oom-22764f31093442a80cbcb2723089c57c6e698ab0
- **Bug**: BUG-2025-002
- **Size**: 184 bytes
- **Impact**: Caused 2GB+ memory consumption
- **Parse time (unmitigated)**: ~7 seconds before OOM
- **Root cause**: Similar exponential backtracking issue

## Usage

These fixtures are used by `sqry-core/tests/oom_prevention_regression.rs` to verify that `SafeParser` properly prevents OOM crashes by enforcing timeouts.

The regression tests will:
1. First look for fixtures in this directory (`test-fixtures/oom-artifacts/`)
2. Fall back to `sqry-core/fuzz/artifacts/` for local development

## Security

These are NOT malware - they are specially crafted inputs that exploit algorithmic complexity in parsers. They do not execute code, modify files, or perform any harmful operations. They simply cause excessive memory allocation during parsing.

## Adding New Artifacts

When fuzzing discovers new OOM artifacts:
1. Copy the artifact to the appropriate subdirectory
2. Name it `oom-<sha1>` where sha1 is from the fuzzer
3. Add a regression test in `oom_prevention_regression.rs`
4. Document the bug in `docs/development/bugs/`
