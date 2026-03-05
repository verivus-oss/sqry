# CLI Integration Planning Template

Use this template when adding or modifying sqry CLI commands, flags, output
formats, or interactive behaviours. Populate it during the planning phase and
store it alongside the component’s planning pack (e.g.,
`docs/development/<component>/CLI_INTEGRATION-_SLUG_.md`).

## 1. Command & UX Summary
- **Command/entry point:** `sqry ...`
- **Primary workflow:** Brief description of the user story this command enables.
- **Semantic search alignment:** How this improves semantic code search.
- **Docs to update/create:** Guides, tutorials, man pages, release notes.

## 2. Inputs & Flags
| Flag/Argument | Type | Default | Required | Description | Notes (validation, conflicts) |
|---------------|------|---------|----------|-------------|-------------------------------|
| `--example`   | str  | `""`    | No       | What it does | Conflicts with `--other`      |

- Enumerate positional arguments, environment variables, and config entries.
- Capture compatibility constraints (e.g., mutually exclusive flags).

## 3. Output & JSON Schema
- **Human-readable output:** Describe format, ordering, truncation rules.
- **JSON schema:** Outline fields emitted under `--json` / streaming modes.
- **Error handling:** Planned exit codes and stderr messaging conventions.
- **Backward compatibility:** Mention any breaking behaviour changes.

## 4. Index & Performance Considerations
- Expected index requirements (e.g., needs symbol index, trigram index).
- Cache interactions or invalidation needs.
- Performance targets (latency, throughput) and measurement approach.

## 5. Test Strategy (Pre-implementation)
- Unit tests (modules/files).
- Integration/e2e tests (`sqry-cli/tests/...`).
- Fixture additions (paths, datasets).
- Manual smoke scenarios (platforms, commands).

## 6. Documentation & Education Plan
- Target guides to author/update **before coding**.
- CLI help text, `--help` examples, changelog entries.
- Rollout/announcement tasks (blog, release notes, internal comms).

## 7. Risks & Open Questions
- Outstanding design choices.
- Dependencies (other features, external crates).
- Rollback/mitigation plan if the feature needs to be disabled.

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


> **Reminder:** Attach the completed template to the planning pack and reference
> it from the Implementation Plan and Test Plan. Update it as decisions evolve
> and keep it in sync with the user-facing documentation.
