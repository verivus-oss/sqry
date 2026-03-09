# Implementation Plan: [Component Name]

🔒 **Status**: Draft
**Created**: YYYY-MM-DD
**Related Docs**: [Spec](01_SPEC-_SLUG_.md), [Design](02_DESIGN-_SLUG_.md)

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
> Rationale: [Steps are clear, testable, and achievable. Each step < 200 LOC.]
> ```

---

## Overview

**Total Estimated LOC**: [estimate]
**Number of Steps**: [count]
**Estimated Time**: [hours/days]

---

## Step 1: [Brief Description]

**Goal**: [What this step achieves]
**Files**: [List of files to create/modify]
**LOC**: ~[estimated lines]

### Changes

- [ ] Create `path/to/file.rs`
- [ ] Modify `path/to/existing.rs` (add [functionality])
- [ ] Add tests in `tests/component_test.rs`

### Code Structure

```rust
// Key types/functions to implement
pub struct NewType {
    // ...
}

pub fn main_function() -> Result<()> {
    // ...
}
```

### Acceptance Criteria

- [ ] AC-1: [Specific, testable criterion]
- [ ] AC-2: [Specific, testable criterion]
- [ ] Tests pass: `cargo test -p sqry-core test_name`

### Dependencies

- [ ] Requires: [Step X to be complete / External dep installed]

---

## Step 2: [Brief Description]

[Same structure as Step 1]

---

## Testing Strategy Per Step

| Step | Unit Tests | Integration Tests | Manual Verification |
|------|------------|-------------------|---------------------|
| 1 | [test names] | [test names] | [manual steps] |
| 2 | [test names] | [test names] | [manual steps] |

---

## Version Bumping

**Commit Types**:
- Step 1-3: `feat(component): ...` → **MINOR bump** (0.1.0 → 0.2.0)
- Bug fixes: `fix(component): ...` → **PATCH bump**

**Final Version**: [0.X.0]

---

## Rollback Plan

If implementation fails:
1. [Rollback step 1]
2. [Rollback step 2]
3. Restore to commit: [hash]

---

## References

- [ ] Spec: docs/development/[component]/01_SPEC-_SLUG_.md
- [ ] Design: docs/development/[component]/02_DESIGN-_SLUG_.md

## Template Instructions (Delete Before Approval)

**How to use this template**:

1. **Fill in all sections** above
2. **Remove all placeholder text** (anything in [brackets])
3. **Delete this "Template Instructions" section**
4. **Verify semantic search litmus test** passes
5. **Self-review** for completeness and clarity
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted
7. **Mark as read-only** (🔒 status at top)
