# 03_VALIDATION-_SLUG_.md — Testing, Reviews & Outcomes (Pilot)

🔄 **Status**: Live (Updated: YYYY-MM-DD HH:MM)

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


## 1. Test Plan Snapshot

| Test Type | Scope | Tools/Commands | Owner |
|-----------|-------|----------------|-------|
| Unit | <modules> | `cargo test -p ...` | <name> |
| Integration | <scenarios> | `cargo test --test ...` | <name> |
| End-to-End / CLI | <cases> | `./target/debug/sqry ...` | <name> |

### Edge Cases & Performance
- **Edge cases**: <list>
- **Performance targets**: <metrics + tools>
- **Fallback behaviour**: <error handling expectations>
- 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

---

## 2. Test Execution Log

| Date | Command | Result | Notes |
|------|---------|--------|-------|
| 2025-10-30 | `cargo test --workspace` | ✅ Pass | coverage 86% |

Attach artefacts (logs, screenshots) under `docs/reviews/<component>/<YYYY-MM-DD>/` and reference here.
- 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

---

## 3. AI Review Summary

### Planning Review
- **Codex review**: <date/link>  
- **Claude review**: <date/link>  
- **Findings**:
  - HIGH: <items or “None”>
  - MEDIUM: <items>
  - LOW: <items>
- **Resolution log**: <notes + links>
- 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

### Implementation Review
- **Codex review**: <date/link>  
- **Claude review**: <date/link>  
- **Findings**:
  - HIGH: <items>
  - MEDIUM: <items>
  - LOW: <items>
- **Resolution log**: <notes + links>

---

## 4. Validation Checklist

- [ ] All acceptance criteria satisfied (link to Section 4 in `01_PLAN-_SLUG_.md`)
- [ ] Tests passing in CI (`cargo test --workspace`)
- [ ] `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`
- [ ] Metrics table updated (authoring time, review iterations, satisfaction)
- [ ] Artefacts archived at `docs/reviews/<component>/<date>/`
- [ ] 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

---

## 5. Participant Survey (Pilot Requirement)

| Participant | Role | Satisfaction (1-5) | Notes / Follow-ups |
|-------------|------|--------------------|--------------------|
| <Name> | Implementer | 5 | <feedback> |
| <Name> | Reviewer | 4 | <feedback> |

Summarize actionable insights and recommendations below.
- 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

---

## 6. Outcome & Recommendation

- **Decision**: Adopt / Iterate / Revert  
- **Rationale**: <summary of data + qualitative feedback>  
- **Follow-up Tasks**: <list of tickets or TODOs>
- 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

---

## References
- Test artefacts: <links>
- Review transcripts: <links>
- Progress doc: `02_EXECUTION-_SLUG_.md`
