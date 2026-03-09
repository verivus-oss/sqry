# Test Plan: [Component Name]

🔒 **Status**: Draft
**Created**: YYYY-MM-DD
**Related**: [Spec](01_SPEC-_SLUG_.md), [Design](02_DESIGN-_SLUG_.md)

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


## Approval

**Approval Status**: ⏸️ Awaiting Approval

> ```markdown
> 🔒 Read-Only (Approved: YYYY-MM-DD by: Your Name)
> Rationale: [Test coverage is comprehensive, all acceptance criteria covered]
> ```

---

## Test Strategy

### Coverage Goals

- **Unit Tests**: >80% line coverage
- **Integration Tests**: All public APIs
- **End-to-End Tests**: Key user workflows
- **Edge Cases**: All known edge cases

### Test Types

| Type | Count | Location | Tools |
|------|-------|----------|-------|
| Unit | [N] | src/[component]/mod.rs | #[test], assert_eq! |
| Integration | [N] | tests/[component]_test.rs | tempfile, fixtures |
| E2E | [N] | tests/e2e/ | CLI invocation |

---

## Unit Tests

### Test 1: [Test Name]

**Purpose**: [What this tests]
**Function**: `test_[function_name]`

```rust
#[test]
fn test_[function_name]() {
    // Arrange
    let input = ...;
    
    // Act
    let result = function(input);
    
    // Assert
    assert_eq!(result, expected);
}
```

**Acceptance Criterion**: [Link to spec AC-X]

---

## Integration Tests

### Test Suite: [Suite Name]

**Tests**:
1. Test interaction between [Component A] and [Component B]
2. Test [workflow description]

**Fixtures**:
- `tests/fixtures/sample_rust.rs` - Sample Rust code
- `tests/fixtures/sample_js.js` - Sample JavaScript

---

## End-to-End Tests

### Scenario: [User Workflow]

**User Story**: [From spec]

**Test Steps**:
1. Run: `sqry [command]`
2. Verify: [expected output]
3. Check: [side effects]

**Expected Outcome**: [What should happen]

---

## Edge Cases

| Case | Input | Expected Output | Test Function |
|------|-------|-----------------|---------------|
| Empty input | `""` | Empty result | `test_empty_input` |
| Unicode | `"函数"` | Parsed correctly | `test_unicode` |
| Large file | 10MB file | No panic, graceful | `test_large_file` |

---

## Performance Tests

### Benchmark: [Operation Name]

**Target**: [metric] < [target value]
**Measurement**: `cargo bench --bench [bench_name]`

**Acceptance**: Must meet target on typical hardware

---

## Regression Tests

If fixing bugs or porting from legacy upstream:
- [ ] Test that previously-working code still works
- [ ] Test that bug is fixed and doesn't regress

---

## Test Execution Plan

### Order of Execution

1. Unit tests first (fast, isolate failures)
2. Integration tests (verify components work together)
3. E2E tests (verify user workflows)
4. Performance tests (verify targets met)

### Continuous Testing

Run after each step:
```bash
cargo test -p sqry-core [test_name]
```

Run all tests before commit:
```bash
cargo test --workspace
```

---

## Acceptance Criteria Mapping

| Spec AC | Test(s) | Status |
|---------|---------|--------|
| AC-1 | test_X, test_Y | ⏸️ Not Run |
| AC-2 | test_Z | ⏸️ Not Run |

**All ACs must have at least one passing test.**

---

## Test Data

### Fixtures

- `tests/fixtures/sample_rust.rs` - Basic Rust with functions, structs
- `tests/fixtures/sample_complex.rs` - Nested structures, generics

### Synthetic Data

- Generate: [What synthetic data, how]

---

## Delete Before Approval

Checklist:
- [ ] All acceptance criteria have tests
- [ ] Edge cases identified and tested
- [ ] Performance targets defined
- [ ] Test data/fixtures prepared
- [ ] Execution plan clear
- [ ] This section deleted

## Template Instructions (Delete Before Approval)

**How to use this template**:

1. **Fill in all sections** above
2. **Remove all placeholder text** (anything in [brackets])
3. **Delete this "Template Instructions" section**
4. **Verify semantic search litmus test** passes
5. **Self-review** for completeness and clarity
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted
7. **Mark as read-only** (🔒 status at top)
