# Test Execution: [Component Name]

🔄 **Status**: Live Document
**Last Updated**: YYYY-MM-DD HH:MM
**Test Plan**: [Link to 05_TEST_PLAN-_SLUG_.md]

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


## Summary

**Total Tests**: [N]
**Passing**: [N]
**Failing**: [N]
**Skipped**: [N]

**Coverage**: [X]% (Target: >80%)

**Result**: ✅ PASS / ❌ FAIL / ⏳ IN PROGRESS

---

## Test Run: YYYY-MM-DD HH:MM

### Environment

- **Rust Version**: `rustc --version`
- **OS**: Linux / macOS / Windows
- **Hardware**: [Brief specs]

### Command

```bash
cargo test --workspace --verbose
```

### Output

```
test result: ok. 42 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.23s
```

### Test Results

| Test | Result | Time | Notes |
|------|--------|------|-------|
| test_symbol_extraction | ✅ PASS | 12ms | - |
| test_empty_input | ✅ PASS | 1ms | - |
| test_unicode | ✅ PASS | 5ms | - |

---

## Acceptance Criteria Verification

| Spec AC | Test(s) | Result | Evidence |
|---------|---------|--------|----------|
| AC-1: [description] | test_X | ✅ PASS | Output shows [expected] |
| AC-2: [description] | test_Y | ✅ PASS | Verified [condition] |

**All acceptance criteria**: ✅ Verified / ❌ Incomplete

---

## Coverage Report

```bash
cargo tarpaulin --workspace --out Html
```

**Results**:
- **Line Coverage**: [X]%
- **Branch Coverage**: [X]%
- **Uncovered Lines**: [list critical uncovered lines, if any]

**Report**: `target/tarpaulin/index.html`

---

## Performance Benchmarks

```bash
cargo bench --bench [bench_name]
```

| Benchmark | Result | Target | Status |
|-----------|--------|--------|--------|
| symbol_extraction_1k_lines | 45ms | <100ms | ✅ PASS |
| index_build_10k_symbols | 2.3s | <10s | ✅ PASS |

---

## Failed Tests (if any)

### Failure 1: test_[name]

**Error**:
```
thread 'test_name' panicked at 'assertion failed: `(left == right)`
  left: `Some(42)`,
 right: `None`', src/component.rs:123:5
```

**Root Cause**: [Analysis]

**Fix Applied**: [What was done]

**Retest Result**: ✅ PASS / ❌ STILL FAILING

---

## Edge Case Results

| Case | Expected | Actual | Status |
|------|----------|--------|--------|
| Empty input | Empty result | Empty result | ✅ |
| Unicode | Parsed correctly | Parsed correctly | ✅ |
| Large file | No panic | No panic | ✅ |

---

## Integration Test Results

### Suite: [Suite Name]

```bash
cargo test --test [integration_test]
```

**Output**:
```
[Full output]
```

**Result**: ✅ All integration tests pass

---

## End-to-End Test Results

### Scenario: [User Workflow]

**Command**:
```bash
./target/debug/sqry [command]
```

**Output**:
```
[Actual output]
```

**Verification**:
- [ ] Output matches expected format
- [ ] Correctness verified
- [ ] Performance acceptable

---

## Issues Found

### Issue 1: [Title]

**Discovered in**: test_[name]
**Severity**: HIGH / MEDIUM / LOW
**Status**: FIXED / OPEN / DEFERRED

**Description**: [What went wrong]

**Resolution**: [How it was fixed, or plan if open]

---

## Regression Check

If porting from legacy upstream or fixing bugs:

- [ ] All previously-passing tests still pass
- [ ] Bug is fixed and doesn't regress
- [ ] No new failures introduced

**Comparison with previous run**:
- Previous: [N] tests, [N] passing
- Current: [N] tests, [N] passing
- **Regression**: None / [List any]

---

## Final Verification

### All Acceptance Criteria Met?

- [ ] AC-1: ✅ Verified in test_X
- [ ] AC-2: ✅ Verified in test_Y
- [ ] AC-3: ✅ Verified in test_Z

### Code Quality Checks

- [ ] `cargo fmt --all --check` ✅ PASS
- [ ] `cargo clippy --all-targets` ✅ PASS (or warnings documented)
- [ ] `cargo build --release` ✅ PASS

### Sign-Off

**Tested by**: [Your Name]
**Date**: YYYY-MM-DD
**Result**: ✅ ALL TESTS PASS - Ready for merge

---

## Update Instructions

After each test run:
1. Update timestamp and test counts
2. Paste full test output
3. Update acceptance criteria verification
4. Document any failures and fixes
5. Update coverage metrics
6. Sign off when all tests pass

## Template Instructions (Delete Before Approval)

**How to use this template**:

1. **Fill in all sections** above
2. **Remove all placeholder text** (anything in [brackets])
3. **Delete this "Template Instructions" section**
4. **Verify semantic search litmus test** passes
5. **Self-review** for completeness and clarity
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted
7. **Mark as read-only** (🔒 status at top)