# Development Process Checklist (Quick Reference)

Date: 2026-03-05 (UTC)
Scope: Applies to all sqry work unless explicitly exempted in AGENTS.md.

1) Decide process
- 6-doc required: new components, >100 LOC refactors, API/architecture changes, plugin system changes.
- Plugin workflow: `PLUGIN_TEMPLATE-_SLUG_.md` + `LANGUAGE_FEASIBILITY_GATE.md`.
- Streamlined pilot workflow (approved components only): `docs/templates/streamlined/` pack.
- Retrospective: legacy upstream ports (Port Plan + tests).
- Optional: <50 LOC bug fixes, docs-only, tests-only (no infra).

2) Create docs in `docs/development/<component>/`
- Use slugged docs:
  - `01_SPEC-_SLUG_.md` (🔒), `02_DESIGN-_SLUG_.md` (🔒), `03_IMPLEMENTATION_PLAN-_SLUG_.md` (🔒)
  - `04_PROGRESS-_SLUG_.md` (🔄), `05_TEST_PLAN-_SLUG_.md` (🔒), `06_TEST_EXECUTION-_SLUG_.md` (🔄)
- Copy from `docs/templates/` and replace `_SLUG_`.
- Add `CLI_INTEGRATION_TEMPLATE-_SLUG_.md` when CLI surface changes.
- Add `MCP_INTEGRATION_TEMPLATE-_SLUG_.md` when MCP tools/resources/prompts/transport/auth/policy change.
- For approved streamlined pilots, use:
  - `01_PLAN-_SLUG_.md`, `02_EXECUTION-_SLUG_.md`, `03_VALIDATION-_SLUG_.md`, and local `MAPPING.md`.

3) Approvals & reviews
- Self-approve Spec/Design/Plan/Test Plan with rationale + date.
- Run UUID review requests for Codex, Gemini, Claude Code (pre- and post-implementation) via your repository review script (for example `scripts/review/request_review_with_uuid.sh`).
- Store review artifacts under `docs/reviews/<component>/<YYYY-MM-DD>/`.
- For each deliverable/task-group apply:
  - 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

4) Implementation guardrails
- Follow docs-first; add/align user guides before coding.
- Tests first; update `04_PROGRESS-_SLUG_.md` after each step.
- Enforce naming rules (`AGENTS.md`); document any exceptions in `03_IMPLEMENTATION_PLAN-_SLUG_.md` + `04_PROGRESS-_SLUG_.md`.

5) Quality gates
- Clippy phased commits: (1) `cargo clippy --all-targets --workspace -- -D warnings`; (2) `cargo clippy --all-targets --workspace`; (3) `cargo clippy --workspace -- -W clippy::pedantic`.
- Run `cargo fmt` + `cargo test --workspace` before reviews/commit.
- Test ignores require reason strings.

6) Traceability
- Update RKG if specs/code/tests change (`cargo run -p graphsync -- --full`).
- Log test results in `06_TEST_EXECUTION-_SLUG_.md`; keep artefacts in docs/reviews.
- For language plugins, ensure capability/scoring/polyglot evidence is reflected in the feasibility gate and plugin template outputs.

7) Release/process alignment (when release-related)
- If release automation/artifacts/signing/provenance changed, update `docs/templates/RELEASE_CHECKLIST.md`.
- Validate checklist items against current `.github/workflows/` behavior (jobs, assets, checksums, verification commands).

8) No shortcuts
- No MVP/partial rollout language, no TODO deferrals, no phased P1-only drops. Ship complete, production-ready features.
