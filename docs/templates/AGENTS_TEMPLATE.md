# AGENTS.md — <PROJECT_NAME> Repository Guide (Template)

## Scope

This file applies to the entire <PROJECT_NAME> repository subtree.
All AI agents (Claude Code, Codex, Gemini, etc.) must follow these rules for every file they touch within this repo.

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


## Mission

Describe the project’s core mission in one or two sentences.
Example: Build a lean, focused <PRIMARY_CAPABILITY> that helps users <OUTCOME>.

Core Philosophy: Do one thing exceptionally well — <PRIMARY_CAPABILITY>.

Deployment Focus: This project targets full product deployment. Avoid POCs/MVP shortcuts—every task should produce a complete, production‑ready improvement.

Not in scope: List the clearly out‑of‑scope categories (e.g., unrelated tools, platforms, or features).

---

## Quick Compliance Checklist

Use this before touching code or docs. Every item must be ✅ or you stop.

| Area | Question | Where to confirm |
| --- | --- | --- |
| Branch | Are you on the correct git branch? | `git branch --show-current` |
| Location | Are you in the correct working directory? | `pwd` (should be `<REPO_ROOT_PATH>`) |
| Folder structure | Does the target directory exist before writing files? | `ls <parent-dir>/` |
| Mission fit | Does the task clearly advance the mission? | `<PHILOSOPHY_DOC>`, spec |
| Planning | Are SPEC/DESIGN/PLAN docs approved (or exception justified)? | `docs/development/<component>/` |
| Docs-first | Is the user-facing guide/help updated or drafted? | Referenced in 01_SPEC + 03_PLAN |
| Tests | Do you have a test plan + required fixtures identified? | `05_TEST_PLAN.md`, `tests/fixtures/` |
| Toolchain | Have you run build, lint, and tests locally? | Local shell history / CI |
| Reviews | Are AI reviews scheduled or exemption logged? | `CODEX_REVIEW.md`, `CODEX_CODE_REVIEW.md` |
| Commit/Versioning | Do you know the correct Conventional Commit type + semver bump? | Section "Commits & Pull Requests" |
| Environment | Do sandbox/approval settings permit the commands you plan to run? | CLI banner / section "Environment & Approvals" |
| File validation | After write/edit: did you verify content with Read tool? | Re‑read edited files |

---

## Repository Layout

Tailor the high‑level layout for your project. Example:

```
<repo>/
├── README.md                      # Project overview
├── IMPLEMENTATION_PLAN.md         # Roadmap
├── AGENTS.md                      # This file
├── docs/                          # Documentation
│   ├── internal/DEVELOPMENT_PROCESS.md   # Development workflow
│   ├── <PHILOSOPHY_DOC>                  # Design philosophy
│   ├── development/<component>/          # Per‑component documentation
│   └── templates/                        # Document templates
├── crates|src|packages|apps/...          # Source code (project‑specific)
└── tests/                                # Integration/E2E tests (optional)
```

Add any domain‑specific folders here (e.g., `plugins/`, `server/`, `cli/`).

---

## Environment & Approvals

The CLI/harness prints `sandbox_mode`, `network_access`, and `approval_policy` at session start. Act accordingly:

| Setting | Meaning | Required behavior |
| --- | --- | --- |
| `sandbox_mode=read-only` | File writes blocked | Request approval before any mutation; prefer planning/documentation work |
| `sandbox_mode=workspace-write` | Only repo + writable roots allowed | Keep edits inside repo; ask before touching other paths |
| `sandbox_mode=danger-full-access` | Full filesystem | Still respect “no destructive commands” rule; double‑check paths |
| `network_access=restricted` | Outbound network blocked | Don’t curl/git clone without approval; lean on local docs |
| `network_access=enabled` | Network permitted | Still avoid unapproved telemetry or uploads |
| `approval_policy=never` | Cannot escalate commands | Find alternatives that stay within sandbox limits |
| `approval_policy=on-request` | You may escalate when justified | Provide one‑sentence justification per command |
| `approval_policy=on-failure` | Retry with escalation only if sandboxed command fails | Capture failure output before re‑running |

If a command is blocked, record the reason and either (a) request escalation (unless policy=never) or (b) revise the plan to keep moving without it.

---

## Toolchain & Builds

Fill in with your project’s primary language(s) and toolchain commands.

- Primary language(s): `<LANGUAGES>`
- Minimum versions: `<MIN_VERSIONS>`
- Build: `<BUILD_CMD>`
- Test: `<TEST_CMD>`
- Format: `<FORMAT_CMD>`
- Lint: `<LINT_CMD>`
- Build binary(ies): `<BUILD_BIN_CMD>`
- Run binary/help: `<RUN_CMD>`

No mixed-language policy (optional): If applicable, define which languages are allowed in production code and where scripts are permitted (e.g., `.github/`, `scripts/`).

---

## Coding Style

Adjust for your stack. Provide language‑specific rules where needed.

- Naming conventions: Modules/functions/variables `<MODULE_VAR_CASE>`, Types `<TYPE_CASE>`, Constants `<CONST_CASE>`, Traits/Interfaces `<TRAIT_CASE>`
- Error handling: Apps use `<APP_ERROR_STYLE>`; libraries use `<LIB_ERROR_STYLE>` (avoid panics in library code)
- Logging: Use `<LOG_LIB>` with `<INIT_INSTRUCTION>`; libraries log with levels, CLI may use user‑facing prints
- Memory/performance: Prefer `<STRING_TYPES/PATTERNS>`; avoid unnecessary allocations; profile before optimizing
- Concurrency: Use `<CONCURRENCY_LIB>` for CPU‑bound tasks; `<LOCKING_LIB>` for synchronization; avoid heavy async runtimes unless I/O bound
- Serialization: Use `<SERDE_EQUIVALENT>`; choose formats (binary `<BINARY_FORMAT>` for internal caches, `<HUMAN_FORMAT>` for debug/CLI)
- Documentation: Public APIs require doc comments; examples for non‑trivial functions; module‑level docs where appropriate

---

## Domain Integrations (Optional)

If your project uses domain frameworks (e.g., AST parsers, ML models, DBs), specify the integration strategy and version pinning here.

- Libraries/Frameworks: `<LIBS_AND_VERSIONS>`
- Strategy: Prefer `<QUERY_BASED_OR_API_PATTERNS>` over manual traversal when possible
- Caching: Cache heavy computations (e.g., parsed trees) with LRU or similar

---

## Plugin/System Architecture (If Applicable)

Describe extension points and required traits/interfaces.

- Plugin trait/interface: Define required methods and metadata
- Built‑in plugins: List core plugins/modules
- External plugins: Document how they are packaged and versioned
- Testing: Include fixtures and performance targets for plugin operations

---

## Core Architecture Principles

Adapt this three‑layer design to your project or replace with your architecture diagram.

```
┌─────────────────────────────┐
│   CLI / UI Layer            │  ← User interaction, argument parsing
└─────────────────────────────┘
            │
            ▼
┌─────────────────────────────┐
│   Core Engine               │  ← Core logic, services, caching, plugin system
└─────────────────────────────┘
            │
            ▼
┌─────────────────────────────┐
│ Extensions / Plugins        │  ← Language/platform/domain integrations
└─────────────────────────────┘
```

Provide module organization and key data structures where useful.

---

## Testing Requirements

- Coverage target: `<COVERAGE_TARGETS>` (e.g., >80% core, 100% critical paths)
- Framework(s): `<TEST_FRAMEWORKS>`
- Test organization: unit (co‑located), integration (`tests/`), end‑to‑end
- Edge cases: Unicode, malformed input, large files, fuzzing, etc.
- Performance tests: Benchmark critical paths (optional)
- Coverage tooling: `<COVERAGE_CMD>`; document path to HTML or summary

---

## Performance Guidelines

Define measurable targets and strategy.

- Targets: `<PERF_TARGETS>` (e.g., P95 latency, memory footprint, binary size)
- Strategy: Profile first; measure before/after; optimize hot paths; incremental work where applicable; cache heavy work; use mmap/streaming for large files

---

## Security & Privacy

- No secrets in commits; respect `.gitignore`
- Local‑first processing (unless explicitly required otherwise)
- No telemetry by default
- Input validation: sanitize paths, handle malformed inputs, limit recursion depth, validate untrusted queries/config before execution

---

## Structured Development Process (Mandatory)

Follow the project’s process guide: `<DEV_PROCESS_PATH>`.

Process selection matrix (customize):

| Change type | Docs required | Reviews required | Notes |
| --- | --- | --- | --- |
| Docs‑only / tests‑only / chore | Update affected docs/tests | Skip AI reviews (per exemption list) | Still run format/test if code touched |
| Bug fix ≤<SMALL_LOC> LOC, no API change | Minimal docs (progress + test execution) | Optional AI reviews | Document why full pack not needed |
| Feature/refactor ><THRESHOLD_LOC> LOC or API‑impacting | Full 6‑doc pack before coding | Dual AI reviews (planning + code) | Include CLI/user‑facing template if applicable |
| Plugin/module work | Simplified 3‑doc pack | Planning + code reviews | Use plugin template |
| Port from <UPSTREAM> | Port plan + retrospective docs | Code review mandatory | Explain deviations from original logic |
| Emergency hotfix | Minimal upfront docs; backfill full docs within 24h | Post‑fix AI review | Only for production outages |

Core rule: No code without specification (exceptions must be explicit and tracked).

---

## AI Planning & Code Review

Use dual‑agent reviews to keep planning and implementation honest. Follow `<DEV_PROCESS_PATH>` for canonical steps.

- Run both planning reviews after self‑approval and again after tests pass
- Archive artefacts under `docs/reviews/<component>/<YYYY-MM-DD>/`
- Treat HIGH and MEDIUM/LOW findings as blocking until addressed or explicitly waived

When to skip AI review: small fixes (<THRESHOLD_LOC> LOC), docs updates, test additions, chores.

---

## Commits & Pull Requests

Conventional Commits (required):

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

Types (tie to semver):
- `feat`: MINOR bump
- `fix`: PATCH bump
- `docs`: no bump
- `test`: no bump
- `refactor`: no bump (unless BREAKING CHANGE)
- `perf`: PATCH bump
- `chore`/`ci`: no bump

Breaking changes: add `BREAKING CHANGE:` footer → MAJOR bump.

Pull Request template: include checkboxes for the 6‑doc set, AI review links, acceptance criteria, and version bump confirmation.

---

## Agent Collaboration Rules

- Planning: Track a concise plan with exactly one `in_progress` task
- Communication: Add brief preambles before tool calls; keep messages concise
- File references: Use clickable paths like `src/lib.rs:42`
- File operations: Read in ≤250‑line chunks; prefer minimal diffs

File Operation Safety Checks (MANDATORY):
1. Verify branch: `git branch --show-current`
2. Verify folder exists before writing: `ls <parent-dir>/`
3. Validate writes by re‑reading changed files
4. Check working directory: ensure `pwd` is `<REPO_ROOT_PATH>`

---

## Scope Discipline

- No drive‑by refactors; no feature creep
- Don’t optimize prematurely; measure first
- Avoid unrelated fixes in the same PR

---

## Out of Scope (Customize)

- List items not aligned with the mission here (e.g., metrics platforms, IDE integrations, unrelated linters)

---

## References

- Development Process: `<DEV_PROCESS_PATH>`
- Implementation Plan: `IMPLEMENTATION_PLAN.md`
- Philosophy: `<PHILOSOPHY_DOC>`
- Templates: `docs/templates/`
- README: `README.md`

---

## Error Handling for Agents

If asked to implement without docs:
```
❌ Cannot proceed. This requires the structured development process.

Based on the request, this is a: [major component / plugin / port]

Required process:
- Major component: Full 6‑document process
- Plugin: Simplified 3‑document process
- Port: Code‑first + retrospective docs

Would you like me to create the documentation?
I'll start with 01_SPEC.md to define what and why.
```

If a feature doesn’t pass the project’s litmus test:
```
⚠️  Feature Review: Does this improve <CORE_MISSION>?

The requested feature "<name>" doesn't clearly serve the project’s mission.

Please clarify how this serves <CORE_MISSION>, or consider building it elsewhere.
```

If tests fail:
```
⚠️  Tests must pass before proceeding.

Action required:
1. Fix the failing tests
2. Run <TEST_CMD> to verify
3. Update 06_TEST_EXECUTION.md with results
4. Then proceed
```

---

## Success Criteria (For Agents)

- All required documents exist and are up‑to‑date
- Tests pass (`<TEST_CMD>`)
- Code formatted (`<FORMAT_CMD>`)
- Lint clean or warnings documented (`<LINT_CMD>`)
- Conventional commits used; version bump correct
- CHANGELOG updated (if automated)
- AI reviews complete; HIGH items addressed
- Litmus test passes for the feature
- Docs links working; no feature creep introduced

---

## Philosophy (Final Reminder)

<PROJECT_NAME> exists to be the best <VALUE_PROPOSITION>.

When in doubt: “Does this make <CORE_MISSION> better?” If yes, do it. If no, don’t.

---

End of template

