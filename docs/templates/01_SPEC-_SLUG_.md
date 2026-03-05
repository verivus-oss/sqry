# Specification: [Component Name]

🔒 **Status**: Draft
**Created**: YYYY-MM-DD
**Author**: [Your Name]
**Phase**: [Phase X from IMPLEMENTATION_PLAN.md]

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


## Approval

> **Self-Approval Instructions**:
> After completing this spec, review it and mark as approved:
>
> ```markdown
> 🔒 Read-Only (Approved: YYYY-MM-DD by: Your Name)
> Rationale: [Why this is necessary, how it serves semantic search, reference to IMPLEMENTATION_PLAN.md]
> ```

**Approval Status**: ⏸️ Awaiting Self-Approval

---

## Problem Statement

**What problem does this solve?**

[Describe the current limitation, pain point, or gap that this component addresses. Be specific about the user impact.]

**Who is affected?**

- [ ] sqry CLI users
- [ ] sqry library users
- [ ] Plugin developers
- [ ] sqry core maintainers

**Current workarounds** (if any):

[Describe how users currently solve this problem, if at all]

---

## Goals

**What will this achieve?**

1. **Goal 1**: [Specific, measurable outcome]
2. **Goal 2**: [Specific, measurable outcome]
3. **Goal 3**: [Specific, measurable outcome]

**Success metrics**:

- [ ] Metric 1: [e.g., "Query time < 100ms"]
- [ ] Metric 2: [e.g., "Test coverage > 80%"]
- [ ] Metric 3: [e.g., "Binary size increase < 5MB"]

---

## Non-Goals

**What is explicitly out of scope?**

1. **Non-Goal 1**: [What we will NOT do] - Rationale: [why not]
2. **Non-Goal 2**: [What we will NOT do] - Rationale: [why not]
3. **Non-Goal 3**: [What we will NOT do] - Rationale: [why not]

---

## Semantic Search Litmus Test

> **Critical Question**: Does this make sqry better at semantic code search?

**Answer**: [Yes / No / Partially]

**Justification**:

[Explain how this component improves semantic code search. If it doesn't directly improve search but enables it (e.g., plugin system), explain the connection.]

**Examples of how this helps users**:

1. **Before**: [Without this component, users have to...]
2. **After**: [With this component, users can...]

**If this doesn't pass the litmus test**: Stop here. Reconsider if this belongs in sqry or should be a separate tool.

---

## User Stories

### Story 1: [Primary Use Case]

**As a** [type of user]
**I want to** [action/capability]
**So that** [benefit/outcome]

**Acceptance Criteria**:
- [ ] Given [context], when [action], then [expected result]
- [ ] Given [context], when [action], then [expected result]

**Example**:
```bash
# Command or code example showing the feature in action
sqry --symbol "process_data"
```

**Expected output**:
```
[What the user should see]
```

---

### Story 2: [Secondary Use Case]

[Repeat structure above]

---

### Story 3: [Edge Case]

[Repeat structure above]

---

## Requirements

### Functional Requirements

| ID | Requirement | Priority | Acceptance Test |
|----|-------------|----------|-----------------|
| FR-1 | [Requirement description] | MUST / SHOULD / COULD | [How to verify] |
| FR-2 | [Requirement description] | MUST / SHOULD / COULD | [How to verify] |
| FR-3 | [Requirement description] | MUST / SHOULD / COULD | [How to verify] |

**Priority definitions**:
- **MUST**: Core functionality, blocking
- **SHOULD**: Important but not blocking
- **COULD**: Nice-to-have, optional

### Non-Functional Requirements

| ID | Requirement | Target | Measurement |
|----|-------------|--------|-------------|
| NFR-1 | Performance: [metric] | [target value] | [How measured] |
| NFR-2 | Memory: [metric] | [target value] | [How measured] |
| NFR-3 | Compatibility: [metric] | [target value] | [How measured] |

**Examples**:
- Performance: Symbol extraction < 100ms for 1000-line files
- Memory: Index size < 10MB for 10K symbols
- Compatibility: Works with tree-sitter 0.20+

---

## Constraints & Assumptions

### Constraints
- **Constraint 1**: [Technical limitation we must work within]
- **Constraint 2**: [Resource limitation]
- **Constraint 3**: [Timeline/scope limitation]

### Assumptions
- **Assumption 1**: [What we're assuming is true]
  - **Validation**: [How we'll verify this assumption]
- **Assumption 2**: [What we're assuming is true]
  - **Validation**: [How we'll verify this assumption]

### Dependencies
- [ ] **Dependency 1**: [Component/feature this depends on]
- [ ] **Dependency 2**: [External library/tool required]

---

## Risks & Mitigations

| Risk | Impact | Probability | Mitigation |
|------|--------|-------------|------------|
| [Risk description] | HIGH/MED/LOW | HIGH/MED/LOW | [How we'll address it] |
| [Risk description] | HIGH/MED/LOW | HIGH/MED/LOW | [How we'll address it] |

**Examples**:
- Risk: Tree-sitter version incompatibility
- Impact: HIGH (breaks plugin system)
- Probability: MEDIUM
- Mitigation: Pin to specific version, extensive testing

---

## Open Questions

1. **Q1**: [Unanswered question]
   - **Answer**: [To be determined / Decision made: X]
2. **Q2**: [Unanswered question]
   - **Answer**: [To be determined / Decision made: X]

**Decision log**: Document major decisions made during spec development here.

---

## References

- **Implementation Plan**: [Link to 03_IMPLEMENTATION_PLAN-_SLUG_.md and milestone context]
- **Related Docs**: [Links to related specs, design docs]
- **External Resources**: [Links to relevant papers, blog posts, etc.]

---

## Appendix

### Example Scenarios

[If helpful, include detailed examples, sample data, or scenarios that illustrate the requirements]

### Glossary

- **Term 1**: [Definition]
- **Term 2**: [Definition]

---

## Template Instructions (Delete Before Approval)

**How to use this template**:

1. **Fill in all sections** above
2. **Remove all placeholder text** (anything in [brackets])
3. **Delete this "Template Instructions" section**
4. **Verify semantic search litmus test** passes
5. **Self-review** for completeness and clarity
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted
7. **Mark as read-only** (🔒 status at top)
8. **Proceed to 02_DESIGN-_SLUG_.md** ONLY after UNCONDITIONAL approval

**Common mistakes**:
- ❌ Vague acceptance criteria ("works well")
- ❌ No measurable success metrics
- ❌ Skipping the semantic search litmus test
- ❌ Unclear or missing non-goals
- ❌ No consideration of risks

**Good practices**:
- ✅ Specific, testable acceptance criteria
- ✅ Clear connection to semantic search mission
- ✅ Realistic constraints and assumptions
- ✅ Documented decision rationale
- ✅ Examples that illustrate the feature
