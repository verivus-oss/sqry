# sqry Document Templates

This directory contains templates for the sqry structured development process.

## Main Process Templates (6-Document Process)

For major components, API changes, and breaking changes:

1. **01_SPEC-_SLUG_.md** - What & Why
   - Problem statement, goals, acceptance criteria
   - Semantic search litmus test
   - User stories and requirements

2. **02_DESIGN-_SLUG_.md** - How
   - Architecture and API design
   - Data structures and algorithms
   - Alternatives considered

3. **03_IMPLEMENTATION_PLAN-_SLUG_.md** - Steps
   - Breakdown into <200 LOC steps
   - Per-step acceptance criteria
   - File paths and dependencies

4. **04_PROGRESS-_SLUG_.md** - Live Status (🔄)
   - Real-time progress tracking
   - Session logs and decisions
   - Blockers and resolutions

5. **05_TEST_PLAN-_SLUG_.md** - Verification Strategy
   - Test types and coverage goals
   - Test cases and edge cases
   - Acceptance criteria mapping

6. **06_TEST_EXECUTION-_SLUG_.md** - Live Results (🔄)
   - Actual test outputs
   - Coverage metrics
   - Acceptance criteria verification

## Special Templates

### PLUGIN_TEMPLATE-_SLUG_.md

Simplified 3-document process for language plugins:
- Specification (what to support)
- Implementation (how it works)
- Tests (verification)

Use for: `sqry-lang-*` plugins

### CLI_INTEGRATION_TEMPLATE-_SLUG_.md

Planning aid for CLI surface changes:
- Command/UX summary and semantic search rationale
- Input/flag catalogue and JSON output schema expectations
- Pre-implementation test strategy and documentation deliverables

Use this template alongside the 6-doc pack whenever a feature touches the CLI.

### MCP_INTEGRATION_TEMPLATE-_SLUG_.md

Planning aid for MCP surface changes:
- Tool/resource/prompt schema contracts and compatibility guarantees
- Transport/auth/safety constraints and runtime behavior
- Cross-language trace continuity expectations and validation strategy

Use this template alongside the 6-doc pack whenever a feature touches `sqry-mcp`.

### Streamlined Pilot Templates (`streamlined/`)

Pilot 3-document planning pack:
- `01_PLAN_TEMPLATE-_SLUG_.md`
- `02_EXECUTION_TEMPLATE-_SLUG_.md`
- `03_VALIDATION_TEMPLATE-_SLUG_.md`
- `MAPPING.md`

Use only for approved streamlined-process pilot work. See `streamlined/README.md`.

## Usage

Replace `_SLUG_` in template filenames with a kebab-case component slug
(for example, `release-pipeline`) before using the files.

Mandatory review rule for all deliverables/task-groups:
6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

### Creating a New Component

```bash
# 1. Create component directory
mkdir -p docs/development/<component-name>

# 2. Copy templates
cp docs/templates/01_SPEC-_SLUG_.md docs/development/<component-name>/01_SPEC-_SLUG_.md
cp docs/templates/02_DESIGN-_SLUG_.md docs/development/<component-name>/02_DESIGN-_SLUG_.md
cp docs/templates/03_IMPLEMENTATION_PLAN-_SLUG_.md docs/development/<component-name>/03_IMPLEMENTATION_PLAN-_SLUG_.md
# ... etc for all 6 docs

# 3. Fill in the templates
# 4. Self-approve each document
# 5. Proceed to implementation
```

### Creating a Plugin

```bash
# 1. Create plugin directory
mkdir -p docs/development/plugins/<language-name>

# 2. Copy plugin template
cp docs/templates/PLUGIN_TEMPLATE-_SLUG_.md docs/development/plugins/<language-name>/PLUGIN_TEMPLATE-_SLUG_.md

# 3. Fill in all 3 sections
# 4. Implement and test
```

## Template Philosophy

Templates exist to:
- ✅ Ensure nothing is forgotten
- ✅ Maintain consistency across components
- ✅ Make self-review systematic
- ✅ Provide clear structure

Templates should NOT:
- ❌ Add unnecessary bureaucracy
- ❌ Slow down development
- ❌ Create documentation for its own sake

**When in doubt**: If a section doesn't apply, write "N/A" and explain why.

## Customization

Feel free to adapt templates for specific needs:
- Remove sections that don't apply (document why)
- Add sections if needed for clarity
- Adjust to component complexity

But maintain the **core structure** to ensure traceability.

## See Also

- **DEVELOPMENT_PROCESS.md** - Full development workflow
- **AGENTS.md** - Agent instructions

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


**Last Updated**: 2026-03-05
