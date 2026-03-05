# Streamlined Planning Pack Templates (Pilot)

## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.

Use these templates when piloting the three-document planning workflow.

## Files
- `01_PLAN_TEMPLATE-_SLUG_.md` — Combines specification and design.
- `02_EXECUTION_TEMPLATE-_SLUG_.md` — Combines implementation plan, progress log, and metrics.
- `03_VALIDATION_TEMPLATE-_SLUG_.md` — Combines test plan, execution logs, AI reviews, and surveys.
- `MAPPING.md` — Reference table mapping the legacy six-document workflow to the new layout.

## Instructions
1. Copy these files into the component directory (e.g., `docs/development/<component>/`) and rename with your slug (e.g., `01_PLAN-_SLUG_.md`).
2. Populate required sections; remove placeholder guidance before marking sections read-only.
3. Archive supporting artefacts under `docs/reviews/<component>/<YYYY-MM-DD>/` and link them from `03_VALIDATION-_SLUG_.md`.
4. Track pilot metrics in `02_EXECUTION-_SLUG_.md` and participant satisfaction in `03_VALIDATION-_SLUG_.md`.
5. After the pilot, summarize findings in `03_VALIDATION-_SLUG_.md §6` and update the active process-governance progress document for the current initiative.
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

> These templates are versioned for the pilot (`v0`). Do not use outside the approved pilot component until the process is formally adopted.
