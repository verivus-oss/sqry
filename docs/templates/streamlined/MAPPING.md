# Streamlined Planning Pack Mapping

## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.

| Legacy Document | New Location | Notes |
|-----------------|--------------|-------|
| `01_SPEC-_SLUG_.md` | `01_PLAN-_SLUG_.md` §1–§4 | Problem statement, goals, acceptance criteria, risks |
| `02_DESIGN-_SLUG_.md` | `01_PLAN-_SLUG_.md` §3 | Architecture diagrams, APIs, alternatives |
| `03_IMPLEMENTATION_PLAN-_SLUG_.md` | `02_EXECUTION-_SLUG_.md` §1 | Step breakdown, acceptance criteria |
| `04_PROGRESS-_SLUG_.md` | `02_EXECUTION-_SLUG_.md` §2–§4 | Progress log, metrics, blockers |
| `05_TEST_PLAN-_SLUG_.md` | `03_VALIDATION-_SLUG_.md` §1 | Test strategy, edge cases, owners |
| `06_TEST_EXECUTION-_SLUG_.md` | `03_VALIDATION-_SLUG_.md` §2 | Command logs, results, coverage notes |
| `CODEX_REVIEW.md` | `03_VALIDATION-_SLUG_.md` §3 (Planning Review) | Include summary + links to artefacts |
| `CODEX_CODE_REVIEW.md` | `03_VALIDATION-_SLUG_.md` §3 (Implementation Review) | Include summary + links to artefacts |

**Artefact Archive**: Continue storing raw outputs under `docs/reviews/<component>/<YYYY-MM-DD>/` and link them from §3 and §4.

**Mandatory Review Rule**: 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.
