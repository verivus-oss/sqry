# sqry Structured Development Process

This repository enforces a 6-document workflow for major components and features. The goal is to ensure clarity before coding, traceable progress, and verifiable outcomes while maintaining sqry's philosophy of **lean, focused development**.

See also:
- `docs/templates/` for document templates
- `docs/templates/RELEASE_CHECKLIST.md` for release pipeline verification requirements
- [`AGENTS.md`](../AGENTS.md) for agent-specific instructions and repository guidelines
- Quick links:
  - [Module & File Naming Standards](#module--file-naming-standards)

---

## Core Philosophy

> "Do one thing exceptionally well - semantic code search"

Every feature must pass the **Semantic Search Litmus Test**:
> "Does this make sqry better at semantic code search?"

If no, reject the feature. This process exists to maintain quality, not to add complexity.

**Deployment Focus**: This workflow supports shipping a full production deployment of sqry. Do not scope work to MCP pilots, MVP experiments, or other stopgaps—plan and deliver as if each feature is going directly to end users.

---

## Core Rule

**No code without specification.** Spec, Design, and Implementation Plan must be written and self-approved (with documented rationale) before any implementation work.

## Ongoing Security Practices (MUST)

Every component owner is responsible for the following recurring security routines:

- **Automated auditing**: Integrate `cargo-audit` into CI/CD so dependency CVEs are surfaced continuously.
  ```bash
  cargo install cargo-audit
  cargo audit
  ```
- **Dependency scanning**: Run `cargo-deny` in automation to flag license, vulnerability, and duplicate dependency problems.
  ```bash
  cargo install cargo-deny
  cargo deny check
  ```
- **Regular updates**: Schedule quarterly dependency reviews, run `cargo update` in an isolated branch, review upstream changelogs for breaking changes, and execute the full `cargo fmt && cargo clippy && cargo test --workspace` loop before landing the updates.
- **Lock file management**: Keep `Cargo.lock` tracked and refreshed so builds remain reproducible across developer machines and CI.

---

## Required Document Structure

Create a folder per component/feature:

```
docs/development/<component-name>/
├── 01_SPEC-_SLUG_.md               (🔒 Read-Only: What & Why)
├── 02_DESIGN-_SLUG_.md             (🔒 Read-Only: How - architecture)
├── 03_IMPLEMENTATION_PLAN-_SLUG_.md (🔒 Read-Only: Steps with acceptance criteria)
├── 04_PROGRESS-_SLUG_.md           (🔄 Live: Real-time status tracking)
├── 05_TEST_PLAN-_SLUG_.md          (🔒 Read-Only: Verification strategy)
└── 06_TEST_EXECUTION-_SLUG_.md     (🔄 Live: Actual test results)
```

**Templates**: Available in `docs/templates/`

## Usage

Replace `_SLUG_` in template filenames with a kebab-case component slug
(for example, `release-pipeline`) before using the files.

---

## When It's Required

### ✅ Required (6-document process)
- **New components** within phases (e.g., "Symbol Extraction System", "AST Query Engine")
- **Major refactors** >100 LOC
- **API changes** that affect public interfaces
- **Plugin system changes**
- **Breaking changes** to architecture

### ⚠️ Simplified (3-document process)
- **Language plugins**: Use plugin template + feasibility gate workflow
  - `docs/templates/PLUGIN_TEMPLATE-_SLUG_.md`
  - `docs/templates/LANGUAGE_FEASIBILITY_GATE.md`
- **Performance optimizations**: Can use abbreviated docs if core logic unchanged

### 🧪 Streamlined Pilot (3-document consolidated pack)
- Use only for approved pilot components:
  - `docs/templates/streamlined/01_PLAN_TEMPLATE-_SLUG_.md`
  - `docs/templates/streamlined/02_EXECUTION_TEMPLATE-_SLUG_.md`
  - `docs/templates/streamlined/03_VALIDATION_TEMPLATE-_SLUG_.md`
  - `docs/templates/streamlined/MAPPING.md`
- Must preserve evidence links under `docs/reviews/<component>/<YYYY-MM-DD>/`.
- Must align naming and acceptance criteria with the canonical process.

### ⏭️ Optional (retrospective docs)
- **Porting from legacy upstream**: Code-first with retrospective docs (since logic exists)
  - Still requires: Port Plan, Test Plan, Test Execution
- **Bug fixes** <50 LOC
- **Documentation updates**
- **Test additions** (not test infrastructure)

---

## Workflow Steps

> **Review Discipline (applies to every coding task)**
>
> - **IMPORTANT:** Take as much time as needed. Be comprehensive. Do NOT rush.
> - **Be thorough:** Read all relevant docs, execute tests, and verify every claim.
> - **Be critical:** Never accept assertions at face value—look for concrete evidence.
> - **Be constructive:** Provide specific, actionable recommendations for any gaps.
> - **Be evidence-based:** Cite files, line numbers, commands, and outputs in all findings (e.g., `src/foo.rs:120`, `cargo test --package foo -- --nocapture`).
> - **Archive your work:** Capture review outputs (e.g., `cargo test --workspace -- --nocapture | tee docs/reviews/<component>/<YYYY-MM-DD>.log`) so others can replay the evidence.
> - **Store artefacts predictably:** Save review notes/logs under `docs/reviews/<component>/<YYYY-MM-DD>/` and reference them from CODEX_REVIEW.md or CODEX_CODE_REVIEW.md.
> - **Close the loop:** Document how HIGH/MEDIUM items were resolved and who was notified (component owner/phase lead) before moving on.
> - **Take your time.** Deliberate reviews are mandatory—silence is not approval; explicitly note “no issues found” with supporting citations when applicable.
> - **Mandatory for every deliverable/task-group:** 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

### Sharing & Reusing Review Artefacts

To help future reviewers diff or re-run prior findings:

- **Folder layout:** Store each review under `docs/reviews/<component>/<YYYY-MM-DD>/`. Include at least `commands.log` (raw terminal output via `tee`), `summary.md` (key findings with citations), and any supporting scripts or data files.
- **Metadata:** At the top of `summary.md`, record reviewer name, date, git commit hash, and the exact commands executed. This makes comparisons reproducible.
- **Diffing guidance:** When repeating a review, copy the earlier artefacts locally and run `diff -u docs/reviews/<component>/<old-date>/commands.log docs/reviews/<component>/<new-date>/commands.log` (or `git diff --no-index`). Log any behaviour changes in the new `summary.md`.
- **Re-run instructions:** If additional tooling was used (e.g., coverage scripts), include shell snippets or helper scripts (`reproduce.sh`) in the same folder so others can re-execute without guesswork.
- **Cross-link:** Reference prior reviews from CODEX_REVIEW.md / CODEX_CODE_REVIEW.md with relative paths (e.g., `See docs/reviews/search-engine/2025-10-03/summary.md for baseline`).

### 1. Detect Component/Feature Request

Watch for requests like:
- "Implement symbol extraction"
- "Add AST query language"
- "Create plugin system"
- "Port search engine from legacy upstream"

### 2. Determine Process Type

**Questions to ask**:
1. Is this a major component (>100 LOC)?
2. Does it change public APIs?
3. Is it a language plugin?
4. Are we porting existing legacy upstream code?

**Decision tree**:
- Major component + new code → Full 6-doc process
- Language plugin → Simplified 3-doc process
- Porting legacy upstream → Code-first + retrospective docs
- Bug fix/small change → Optional process

### 3. Create Documentation Structure

**SAFETY CHECKS (perform BEFORE creating files)**:
```bash
# 1. Verify correct branch
git branch --show-current

# 2. Verify working directory
pwd  # Should be /srv/repos/internal/verivusai-labs/sqry

# 3. Create directory structure
mkdir -p docs/development/<component-name>

# 4. Verify directory was created
ls docs/development/<component-name>/

# 5. Copy templates
cp docs/templates/01_SPEC-_SLUG_.md docs/development/<component-name>/01_SPEC-_SLUG_.md
cp docs/templates/02_DESIGN-_SLUG_.md docs/development/<component-name>/02_DESIGN-_SLUG_.md
cp docs/templates/03_IMPLEMENTATION_PLAN-_SLUG_.md docs/development/<component-name>/03_IMPLEMENTATION_PLAN-_SLUG_.md
cp docs/templates/04_PROGRESS-_SLUG_.md docs/development/<component-name>/04_PROGRESS-_SLUG_.md
cp docs/templates/05_TEST_PLAN-_SLUG_.md docs/development/<component-name>/05_TEST_PLAN-_SLUG_.md
cp docs/templates/06_TEST_EXECUTION-_SLUG_.md docs/development/<component-name>/06_TEST_EXECUTION-_SLUG_.md

# 6. Validate files were created
ls -la docs/development/<component-name>/

# For plugins
mkdir -p docs/development/plugins/<language-name>
ls docs/development/plugins/<language-name>/  # Verify
cp docs/templates/PLUGIN_TEMPLATE-_SLUG_.md docs/development/plugins/<language-name>/PLUGIN_TEMPLATE-_SLUG_.md
cp docs/templates/LANGUAGE_FEASIBILITY_GATE.md docs/development/plugins/<language-name>/LANGUAGE_FEASIBILITY_GATE.md
```

**File Write Validation Protocol**:
After EVERY file write or edit:
1. Use `Read` tool to verify content was written correctly
2. Check file size is non-zero: `ls -lh <file-path>`
3. Verify file is in the expected location
4. For edits, confirm the specific changes were applied

**Document status markers**:
- Read-only docs: `🔒 Read-Only (Approved: YYYY-MM-DD by: <name> - Rationale: <reason>)`
- Live docs: `🔄 Live (Updated: YYYY-MM-DD HH:MM)`

### 3a. CLI Integration Template (when applicable)

Any change that adds/modifies CLI commands, flags, output formats, or interactive
behaviour **must** copy `docs/templates/CLI_INTEGRATION_TEMPLATE-_SLUG_.md` into the
component directory (for example,
`docs/development/<component>/CLI_INTEGRATION-_SLUG_.md`) and complete it *before*
finalising the Design and Implementation Plan. Treat the filled template as a
living reference that stays in sync with the rest of the planning pack.

Key expectations:
- Capture UX decisions, JSON schema, and error handling up front.
- Enumerate required docs/guide updates.
- Feed the test strategy directly into `05_TEST_PLAN-_SLUG_.md`.

Planning reviews will block if the CLI template is missing or incomplete.

### 3aa. MCP Integration Template (when applicable)

Any change that adds/modifies MCP tools, resources, prompts, transport,
authentication, or safety policy **must** copy
`docs/templates/MCP_INTEGRATION_TEMPLATE-_SLUG_.md` into the component
directory (for example,
`docs/development/<component>/MCP_INTEGRATION-_SLUG_.md`) and
complete it before finalising Design and Implementation Plan.

Planning reviews will block if the MCP template is missing or incomplete for
MCP-scoped work.

### 3ab. Release Checklist Template (when applicable)

Any change that affects release automation, artifact layout, signing,
provenance, or distribution verification must update
`docs/templates/RELEASE_CHECKLIST.md` and explicitly validate checklist items
against the current workflow definitions in `.github/workflows/`.

Release-related planning/code reviews should reject changes when the checklist
no longer matches the actual jobs, artifacts, or verification commands.

### 3ac. Streamlined Pilot Setup (approved components only)

For approved pilot components using the consolidated 3-document pack:

```bash
mkdir -p docs/development/<component-name>
cp docs/templates/streamlined/01_PLAN_TEMPLATE-_SLUG_.md docs/development/<component-name>/01_PLAN-_SLUG_.md
cp docs/templates/streamlined/02_EXECUTION_TEMPLATE-_SLUG_.md docs/development/<component-name>/02_EXECUTION-_SLUG_.md
cp docs/templates/streamlined/03_VALIDATION_TEMPLATE-_SLUG_.md docs/development/<component-name>/03_VALIDATION-_SLUG_.md
cp docs/templates/streamlined/MAPPING.md docs/development/<component-name>/MAPPING.md
```

- Use `MAPPING.md` to preserve traceability to the canonical 6-document workflow.
- Keep review/test artefacts under `docs/reviews/<component>/<YYYY-MM-DD>/`.
- 6. **Submit for review to all the available llm providers** iterate until ALL items have been addressed and UNCONDITIONAL approval is granted.

### 3b. Docs-First Development (TDD for Guides)

For every user-facing feature (CLI, IDE, guides, API surface):
- Draft or update the relevant user guide/tutorial **before writing code**.
- Link the guide in `01_SPEC-_SLUG_.md` (Goals) and `03_IMPLEMENTATION_PLAN-_SLUG_.md` (Steps).
- Reference the planned guide updates in the CLI template (if applicable).
- Record guide completion in `04_PROGRESS-_SLUG_.md` prior to implementation sign-off.

This ensures documentation drives implementation (docs-as-tests) and enforces a
consistent, education-first workflow.

### 3c. Module & File Naming Standards

Consistent naming eliminates clippy `module_inception` warnings and keeps the
architecture readable.

- **Module paths describe domains**: Use descriptive snake_case file names such as
  `code_graph.rs`, `graph_indices.rs`, or `call_sites.rs`. Avoid repeating the exact
  PascalCase type in the file/module name (e.g., prefer `code_graph` over `graph` for
  `CodeGraph`).
- **Role-based modules are fine**: Files named after responsibilities (`builder.rs`,
  `call_sites.rs`, `edge.rs`) remain acceptable when the exported type includes a role
  suffix like `GraphBuilder` or `CodeEdge`; the rule only forbids exact repeats such as
  `graph.rs` containing `CodeGraph`.
- **Types stay PascalCase**: Keep exported structs/enums/traits in PascalCase (`CodeGraph`,
  `GraphIndices`, `GraphBuilder`) even when their module uses a prefixed variant.
- **Function names remain verbs**: Use snake_case verbs for functions (`link_typescript_javascript_imports`,
  `normalize_path`) so call sites clearly signal behavior.
- **Document decisions**: Capture planned module/file names in `03_IMPLEMENTATION_PLAN-_SLUG_.md` so reviewers can
  catch collisions early, and reference any exceptions (e.g., legacy paths) in `04_PROGRESS-_SLUG_.md`.
- **No new `#[allow(clippy::module_inception)]`**: If a conflict appears, rename the module or the
  exported type instead of adding new allow attributes. Existing allowances must be scheduled for cleanup.

### 3d. Variable Naming Standards

Variable naming aligns with our semantic-search mission and must follow `AGENTS.md`.

- **Apply the schema everywhere**: Local variables, struct fields, and function parameters must use the prescribed prefixes/suffixes for booleans, collections, identifiers, caches, and tree-sitter constructs.
- **Plan deviations**: If an external API or serde mapping forces a different name, document the exception in `03_IMPLEMENTATION_PLAN-_SLUG_.md` and track the cleanup in `04_PROGRESS-_SLUG_.md`.
- **Review enforcement**: Planning and code reviews should reject changes that ignore the convention unless the exception is explicitly approved.
- **Distinction from clippy lints**: Variable naming conventions address project-wide patterns. Clippy's `similar_names` lint addresses confusingly similar variables in the same scope. These are separate concerns.

### 3e. Code Quality & Test Infrastructure Standards

#### Clippy Pedantic Compliance

sqry targets clippy pedantic compliance to maintain high code quality. While not blocking (tests can pass with pedantic warnings), systematic cleanup is expected.

**Workflow**:
1. Run pedantic checks: `cargo clippy --workspace -- -W clippy::pedantic`
2. Apply safe auto-fixes: `cargo clippy --fix --allow-dirty -- -W clippy::pedantic`
3. Manually address semantic issues (`similar_names`, `module_inception`, documentation gaps)
4. Document suppressions with inline comments explaining rationale
5. Track progress in `docs/development/` analysis documents for systematic cleanup

**Common patterns to address**:
- **similar_names**: Rename for semantic clarity (e.g., `context` → `semantic_context` when `content` is in scope)
- **module_inception**: Rename module or type to avoid `module::Module` anti-pattern
- **missing_errors_doc/missing_panics_doc**: Add `# Errors` and `# Panics` documentation sections
- **must_use_candidate**: Add `#[must_use]` to functions whose return values shouldn't be ignored

**When to create analysis documents**:
- After major refactors affecting >50 files
- When starting systematic pedantic cleanup campaigns
- Include: baseline metrics, warning categories, improvement roadmap, verification commands

#### Test Infrastructure Requirements

**Binary path resolution** (integration tests):
- Never access `CARGO_BIN_EXE_*` environment variables directly
- Create common test helper modules (e.g., `tests/common/mod.rs`) with fallback logic
- Pattern must work in both CI (env var set) and local workspace contexts (fallback to `target/debug` or `target/release`)

Example helper:
```rust
pub fn sqry_bin() -> PathBuf {
    std::env::var("CARGO_BIN_EXE_sqry")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let workspace_dir = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();
            let debug_path = workspace_dir.join("target/debug/sqry");
            let release_path = workspace_dir.join("target/release/sqry");
            if debug_path.exists() { debug_path }
            else if release_path.exists() { release_path }
            else { panic!("Could not find sqry binary..."); }
        })
}
```

**Test isolation** (environment variables, global state):
- Use `serial_test` crate for tests that modify global state
- Add `serial_test = "3.0"` to crate's dev-dependencies
- Mark tests with `#[serial]` attribute to prevent parallel execution interference
- Document why serialization is needed (e.g., "Modifies environment variables")

**Test helper modules**:
- Keep complete helper modules - avoid partial implementations
- Mark shared utilities with `#[allow(dead_code)]` to suppress false warnings
- Include standard helpers: fixture paths, test server setup, index building, binary location
- Document each helper's purpose in module-level comments

**Test ignore reasons** (mandatory):
- All `#[ignore]` attributes must include explanatory reasons
- Format: `#[ignore = "reason"]`
- Common reasons:
  - `"Integration test - run in nightly job to keep CI fast"`
  - `"Performance test - run in nightly job to keep CI fast"`
  - `"Expensive rebuild test - enable for validation testing"`
  - `"Requires external service - run manually when available"`

**Planning integration**:
- Document test infrastructure patterns in `05_TEST_PLAN-_SLUG_.md`
- Include helper module creation in `03_IMPLEMENTATION_PLAN-_SLUG_.md` steps
- Record test isolation needs (serial_test, fixtures, env vars) in test plan
- Update `04_PROGRESS-_SLUG_.md` when adding new test infrastructure

### 4. Specification (`01_SPEC-_SLUG_.md`)

**Contents**:
- **Problem statement**: What problem does this solve?
- **Goals**: What will this achieve?
- **Non-goals**: What is explicitly out of scope?
- **User stories**: How will developers use this?
- **Acceptance criteria**: How do we know it's done?
- **Semantic search litmus test**: How does this improve semantic search?

**Self-approval**:
- Document your rationale for why this is necessary
- Reference 03_IMPLEMENTATION_PLAN-_SLUG_.md milestone context
- Confirm it passes the semantic search litmus test
- Mark as approved with date and rationale

**Example approval**:
```markdown
🔒 Read-Only (Approved: 2025-10-01 by: Maintainer)
Rationale: Symbol extraction is core to Phase 1 (Weeks 2-3) and essential for AST-based search.
Passes semantic search test: enables finding symbols by semantic meaning, not text.
```

### 5. Design (`02_DESIGN-_SLUG_.md`)

**Contents**:
- **Architecture**: ASCII diagrams showing components
- **API signatures**: Public interfaces and traits
- **Data structures**: Key types and their relationships
- **Alternatives considered**: What else did we consider and why did we reject it?
- **Integration points**: How does this fit with existing code?
- **Plugin compatibility**: If applicable, how does this work with plugins?

**Self-approval**:
- Verify design aligns with 3-layer architecture (CLI, Core, Plugins)
- Check that it doesn't introduce feature bloat
- Document trade-offs and rationale

### 6. Implementation Plan (`03_IMPLEMENTATION_PLAN-_SLUG_.md`)

**Contents**:
- **Steps**: Break into <200 LOC chunks
- **Acceptance criteria**: Per-step verification
- **File paths**: Exact files to create/modify
- **Dependencies**: What must be done first?
- **Testing strategy**: How to verify each step?

**Self-approval**:
- Verify each step has clear acceptance criteria
- Ensure test coverage is planned from day 1
- Document any risks or unknowns

### 7. Test Plan (`05_TEST_PLAN-_SLUG_.md`)

**Contents**:
- **Test strategy**: Unit, integration, end-to-end
- **Test cases**: Enumerate all cases to cover
- **Edge cases**: Boundary conditions and error cases
- **Performance targets**: If applicable
- **Test coverage goals**: Aim for >80% for core, 100% for critical paths
- **Test infrastructure needs**: Document requirements for:
  - Binary path resolution (if integration tests need CLI binary)
  - Test isolation (if tests modify global state - require serial_test)
  - Common test helpers (fixture paths, test servers, index building)
  - Test ignore reasons (for expensive/slow tests)

**Note**: Test plan is created BEFORE implementation, but execution happens during/after.

**Reference**: See [Code Quality & Test Infrastructure Standards](#3e-code-quality--test-infrastructure-standards) for detailed patterns.

### 8. AI Planning Review (CODEX + Gemini + Claude Code)

After Spec/Design/Plan/TestPlan are self-approved, get AI review using the **UUID-based workflow**:

> **Canonical guidance:** Treat this section as the source of truth for review workflows. Use the UUID-based script to ensure complete audit trail and automatic iteration tracking.

#### Prerequisites

**UUID CLI Tool** (one-time setup):
```bash
# Install uuid CLI tool
cargo install uuid-cli

# Verify installation
uuid --version
```

**Gemini CLI Configuration** (one-time setup):

> **IMPORTANT**: Gemini CLI requires WriteFileTool configuration to write review files automatically. Without this, review output files will be empty (0 bytes).

Configure `~/.gemini/settings.json` with the following:

```json
{
  "ide": {
    "enabled": true,
    "hasSeenNudge": true
  },
  "security": {
    "auth": {
      "selectedType": "oauth-personal"
    }
  },
  "tools": {
    "coreTools": [
      "EditTool", "GlobTool", "WebSearchTool", "ReadFileTool",
      "LSTool", "ReadManyFilesTool", "MemoryTool", "GrepTool",
      "ShellTool", "WebFetchTool", "WriteFileTool"
    ],
    "autoAccept": true
  },
  "approvalMode": "auto_edit"
}
```

**Backup your settings before modifying**:
```bash
cp ~/.gemini/settings.json ~/.gemini/settings.json.backup.$(date +%Y%m%d_%H%M%S)
```

**Troubleshooting**:
- If Gemini review files are empty (0 bytes), verify WriteFileTool is in settings.json
- Verify Gemini CLI version >= 0.17.0 has WriteFileTool support
- See `docs/templates/README.md` for complete troubleshooting guide
- The review script uses `--approval-mode=yolo` to auto-approve file writes

#### UUID-Based Planning Review Workflow

**Step 1: Request Reviews from All Three Agents**

Use the UUID script to request reviews from Codex, Gemini, and Claude Code. The script automatically:
- Generates unique UUIDv7 for each review
- Detects iteration number (iter1, iter2, etc.)
- Creates UUID-based request and output files
- Updates output paths in request documents
- Preserves complete audit trail (no overwrites)

```bash
# Request Codex review (Technical Arbiter)
./scripts/review/request_review_with_uuid.sh \
  --agent codex \
  --request docs/development/<component>/<name>_review_request_pre_codex.md

# Request Gemini review (Alternative Perspective)
./scripts/review/request_review_with_uuid.sh \
  --agent gemini \
  --request docs/development/<component>/<name>_review_request_pre_gemini.md

# Request Claude Code review (Implementation Validation)
./scripts/review/request_review_with_uuid.sh \
  --agent claude \
  --request docs/development/<component>/<name>_review_request_pre_claude.md
```

**Step 2: Read Review Outputs**

The script creates UUID-based output files. Find the latest iteration reviews:

```bash
# Find latest Codex review (sorted by UUID timestamp)
ls -lt docs/development/<component>/<name>_review_pre_codex_iter*.md | head -1

# Find latest Gemini review
ls -lt docs/development/<component>/<name>_review_pre_gemini_iter*.md | head -1

# Find latest Claude Code review
ls -lt docs/development/<component>/<name>_review_pre_claude_iter*.md | head -1

# Read specific iteration (example: iter2)
cat docs/development/<component>/<name>_review_pre_codex_iter2_019ab41e-9d2a-7000-b9c5-f09209a8e5ad.md
cat docs/development/<component>/<name>_review_pre_gemini_iter2_019ab420-7287-7000-a3d5-d5227efa6d59.md
cat docs/development/<component>/<name>_review_pre_claude_iter2_019ab422-1a3b-7000-c4f6-2b8a9c7e5f1d.md
```

**Step 3: Review File Structure**

After running the UUID script, you'll have:

```
docs/development/<component>/
├── <name>_PLAN.md                                    # Template (no UUID)
├── <name>_review_request_pre_codex.md                # Template (no UUID)
├── <name>_review_request_pre_gemini.md               # Template (no UUID)
├── <name>_review_request_pre_claude.md               # Template (no UUID)
├── <name>_review_request_pre_codex_iter1_019ab405-5a52-7000-b550-91d2ba0045ce.md    # iter1 request (UUID)
├── <name>_review_pre_codex_iter1_019ab405-5a52-7000-b550-91d2ba0045ce.md             # iter1 review (UUID)
├── <name>_review_request_pre_gemini_iter1_019ab407-4ef7-7000-905e-4334cdde551e.md   # Gemini iter1 request
├── <name>_review_pre_gemini_iter1_019ab407-4ef7-7000-905e-4334cdde551e.md            # Gemini iter1 review
├── <name>_review_request_pre_claude_iter1_019ab409-2b8f-7000-a1c3-7e5d9f4a8b2c.md   # Claude iter1 request
├── <name>_review_pre_claude_iter1_019ab409-2b8f-7000-a1c3-7e5d9f4a8b2c.md            # Claude iter1 review
├── <name>_review_request_pre_codex_iter2_019ab41e-9d2a-7000-b9c5-f09209a8e5ad.md    # iter2 request (UUID)
├── <name>_review_pre_codex_iter2_019ab41e-9d2a-7000-b9c5-f09209a8e5ad.md             # iter2 review (UUID)
├── <name>_review_request_pre_gemini_iter2_019ab420-7287-7000-a3d5-d5227efa6d59.md   # Gemini iter2 request
├── <name>_review_pre_gemini_iter2_019ab420-7287-7000-a3d5-d5227efa6d59.md            # Gemini iter2 review
├── <name>_review_request_pre_claude_iter2_019ab422-1a3b-7000-c4f6-2b8a9c7e5f1d.md   # Claude iter2 request
└── <name>_review_pre_claude_iter2_019ab422-1a3b-7000-c4f6-2b8a9c7e5f1d.md            # Claude iter2 review
```

**Benefits of UUID-Based Workflow**:
- ✅ Complete audit trail - every iteration preserved with unique UUID
- ✅ No overwrites - previous reviews never lost
- ✅ Time-sortable - UUIDv7 includes timestamp for chronological ordering
- ✅ Conflict-free - multiple agents/iterations never collide
- ✅ Traceable - UUID links request → output → iteration

**Document findings**:
- Consolidate findings from all three agents in `docs/development/<component>/CONSOLIDATED_REVIEW.md`
- Reference specific UUID-based review files for each agent's feedback
- Categorize: HIGH (must address), MEDIUM (should address), LOW (nice to have)
- **Do not begin implementation until all HIGH items are resolved across all three agents**
- Re-run reviews after addressing feedback using the same UUID script (automatically creates iter2, iter3, etc.)
- Document decisions on MEDIUM/LOW items (accept, defer, or reject with rationale)

### 9. Implementation with Progress Tracking (`04_PROGRESS-_SLUG_.md`)

**During implementation**:
- Update `04_PROGRESS-_SLUG_.md` after each step
- Statuses: ✅ Done, ⏳ In Progress, 🚫 Blocked, ⏸️ Not Started
- Record decisions, blockers, and design changes
- Update at least once per working session

**Clippy compliance (MANDATORY - 3 phased commits)**:

Implementation is **NOT complete** until all three clippy phases are committed:

| Phase | Command | When to Commit |
|-------|---------|----------------|
| **1. Errors** | `cargo clippy --all-targets --workspace -- -D warnings` | With implementation (first commit) |
| **2. Warnings** | `cargo clippy --all-targets --workspace` | Separate commit after Phase 1 |
| **3. Pedantic** | `cargo clippy --workspace -- -W clippy::pedantic` | Separate commit after Phase 2 |

**Code quality checks during implementation**:
- Phase 1 (errors) MUST pass before first commit
- Follow test infrastructure patterns (see [§3e](#3e-code-quality--test-infrastructure-standards))
- Document any necessary `#[allow]` attributes with inline comments
- Create test helper modules for integration tests requiring binary paths
- Use `#[serial]` for tests modifying global state

**Commit strategy**:
```bash
# Reference the implementation plan
git commit -m "feat(symbols): implement step 1 - basic symbol extraction

Implements symbol extraction from Rust AST nodes.
See: docs/development/ARCHIVE/symbol-extraction/03_IMPLEMENTATION_PLAN-_SLUG_.md#step-1

- Added SymbolExtractor trait
- Implemented for Rust language
- Added unit tests for function extraction"
```

**Version bumping during implementation** (see semver section below):
- **feat**: Bump minor version (0.1.0 → 0.2.0)
- **fix**: Bump patch version (0.1.0 → 0.1.1)
- **BREAKING CHANGE**: Bump major version (0.1.0 → 1.0.0)

### 10. Test Execution (`06_TEST_EXECUTION-_SLUG_.md`)

**Run tests frequently**:
```bash
# Run all tests
cargo test --workspace

# Run specific component tests
cargo test -p sqry-core --lib symbols

# Run with coverage (if configured)
cargo tarpaulin --workspace
```

**Document in `06_TEST_EXECUTION-_SLUG_.md`**:
- Test command used
- Full output (pass/fail for each test)
- Coverage metrics
- Verification of each acceptance criterion from SPEC
- Any test failures with analysis

**Required**: All tests must pass before marking component as complete.

### 11. AI Code Review (CODEX + Gemini + Claude Code)

After implementation is complete and tests pass, use the **UUID-based workflow** for post-implementation reviews:

#### UUID-Based Post-Implementation Review Workflow

**Step 1: Request Post-Implementation Reviews from All Three Agents**

```bash
# Request Codex post-implementation review (Technical Arbiter)
./scripts/review/request_review_with_uuid.sh \
  --agent codex \
  --request docs/development/<component>/<name>_review_request_post_codex.md

# Request Gemini post-implementation review (Alternative Perspective)
./scripts/review/request_review_with_uuid.sh \
  --agent gemini \
  --request docs/development/<component>/<name>_review_request_post_gemini.md

# Request Claude Code post-implementation review (Implementation Validation)
./scripts/review/request_review_with_uuid.sh \
  --agent claude \
  --request docs/development/<component>/<name>_review_request_post_claude.md
```

**Step 2: Read Post-Implementation Review Outputs**

```bash
# Find latest Codex post-implementation review
ls -lt docs/development/<component>/<name>_review_post_codex_iter*.md | head -1

# Find latest Gemini post-implementation review
ls -lt docs/development/<component>/<name>_review_post_gemini_iter*.md | head -1

# Find latest Claude Code post-implementation review
ls -lt docs/development/<component>/<name>_review_post_claude_iter*.md | head -1

# Example: Read iter2 reviews
cat docs/development/<component>/<name>_review_post_codex_iter2_019ab446-0f6a-7000-bd9e-8d535d2d6956.md
cat docs/development/<component>/<name>_review_post_gemini_iter2_019ab448-2c7b-7000-c5d7-3f9b0e8a6d3e.md
cat docs/development/<component>/<name>_review_post_claude_iter2_019ab44a-4e8c-7000-d6e8-4g0c1f9b7e4f.md
```

**Step 3: Post-Implementation File Structure**

```
docs/development/<component>/
├── <name>_review_request_post_codex.md                # Template (no UUID)
├── <name>_review_request_post_gemini.md               # Template (no UUID)
├── <name>_review_request_post_claude.md               # Template (no UUID)
├── <name>_review_request_post_codex_iter1_019ab440-1a2b-7000-a9c1-6e7d8f9a0b1c.md    # iter1 request (UUID)
├── <name>_review_post_codex_iter1_019ab440-1a2b-7000-a9c1-6e7d8f9a0b1c.md             # iter1 review (UUID)
├── <name>_review_request_post_gemini_iter1_019ab442-3c4d-7000-b0d2-7f8e9g0b1c2d.md   # Gemini iter1 request
├── <name>_review_post_gemini_iter1_019ab442-3c4d-7000-b0d2-7f8e9g0b1c2d.md            # Gemini iter1 review
├── <name>_review_request_post_claude_iter1_019ab444-5e6f-7000-c1e3-8g9f0h1c2d3e.md   # Claude iter1 request
├── <name>_review_post_claude_iter1_019ab444-5e6f-7000-c1e3-8g9f0h1c2d3e.md            # Claude iter1 review
├── <name>_review_request_post_codex_iter2_019ab446-0f6a-7000-bd9e-8d535d2d6956.md    # iter2 request (UUID)
├── <name>_review_post_codex_iter2_019ab446-0f6a-7000-bd9e-8d535d2d6956.md             # iter2 review (UUID)
├── <name>_review_request_post_gemini_iter2_019ab448-2c7b-7000-c5d7-3f9b0e8a6d3e.md   # Gemini iter2 request
├── <name>_review_post_gemini_iter2_019ab448-2c7b-7000-c5d7-3f9b0e8a6d3e.md            # Gemini iter2 review
├── <name>_review_request_post_claude_iter2_019ab44a-4e8c-7000-d6e8-4g0c1f9b7e4f.md   # Claude iter2 request
└── <name>_review_post_claude_iter2_019ab44a-4e8c-7000-d6e8-4g0c1f9b7e4f.md            # Claude iter2 review
```

**Document findings**:
- Consolidate findings from all three agents in `docs/development/<component>/CONSOLIDATED_CODE_REVIEW.md`
- Reference specific UUID-based review files for each agent's feedback
- **Do not merge until all HIGH items are resolved across all three agents**
- Re-run reviews after fixes using the same UUID script (automatically creates iter2, iter3, etc.)
- Document decisions on MEDIUM/LOW items (accept, defer, or reject with rationale)

### 12. Pull Request / Merge

**PR checklist**:
- [ ] All 6 documents exist and are up-to-date
- [ ] CODEX_REVIEW.md and CODEX_CODE_REVIEW.md document AI feedback
- [ ] HIGH priority recommendations addressed (or reasoned exceptions documented)
- [ ] All tests passing (`cargo test --workspace`)
- [ ] All acceptance criteria verified (checklist in PR description)
- [ ] Version bumped appropriately (semver)
- [ ] CHANGELOG.md updated

**PR template**:
```markdown
## Component: <name>

### Documentation
- Spec: `docs/development/<component>/01_SPEC-_SLUG_.md`
- Design: `docs/development/<component>/02_DESIGN-_SLUG_.md`
- Implementation Plan: `docs/development/<component>/03_IMPLEMENTATION_PLAN-_SLUG_.md`
- Progress: `docs/development/<component>/04_PROGRESS-_SLUG_.md`
- Test Plan: `docs/development/<component>/05_TEST_PLAN-_SLUG_.md`
- Test Execution: `docs/development/<component>/06_TEST_EXECUTION-_SLUG_.md`
- CODEX Planning Review: `docs/development/<component>/CODEX_REVIEW.md`
- CODEX Code Review: `docs/development/<component>/CODEX_CODE_REVIEW.md`

### Acceptance Criteria
- [ ] Criterion 1 from SPEC (verified in test X)
- [ ] Criterion 2 from SPEC (verified in test Y)
...

### Version Bump
- Previous: 0.1.0
- New: 0.2.0
- Type: Minor (new feature)

### Test Results
All tests passing. Coverage: 85% (target: >80%).
See TEST_EXECUTION.md for details.
```

---

## Special Cases

### Porting from legacy upstream (Code-First Approach)

When porting existing working code from legacy upstream:

1. **Create abbreviated docs**:
   - `PORT_PLAN.md`: What to port, how to adapt, mapping of legacy upstream → sqry
   - `TEST_PLAN.md`: How to verify ported code works
   - `TEST_EXECUTION.md`: Actual test results

2. **Port the code**: Since logic already exists and works

3. **Write retrospective docs** (after porting):
   - `01_SPEC-_SLUG_.md`: Document what the component does (retrospective)
   - `02_DESIGN-_SLUG_.md`: Document architecture as implemented
   - `03_IMPLEMENTATION_PLAN-_SLUG_.md`: Document what was actually done

4. **Self-approval**: Approve retrospective docs with rationale

**Example**:
```markdown
🔒 Read-Only (Approved: 2025-10-02 by: Maintainer - Retrospective)
Rationale: Ported working symbol extraction from legacy upstream. Logic proven in production.
Changes made: Simplified to remove feature bloat, adapted to plugin architecture.
Tests verify functional equivalence with legacy upstream.
```

### Language Plugins (Template + Feasibility Gate Workflow)

Language plugins use the plugin-specific workflow:

1. Complete `PLUGIN_TEMPLATE-_SLUG_.md` from `docs/templates/PLUGIN_TEMPLATE-_SLUG_.md`.
2. Complete language gate from `docs/templates/LANGUAGE_FEASIBILITY_GATE.md`.
3. Ensure plugin tests and evidence are captured per template sections.

See `docs/templates/PLUGIN_TEMPLATE-_SLUG_.md` for the template.

**Rationale**: Plugins follow a template pattern, so full 6-doc process is overkill.

---

### Extending Relation Tracking to Additional Languages

Rust currently ships relation tracking; the README outlines Python, JavaScript, TypeScript, and Go as the next targets. Each port touches shared indexing logic and per-language plugins, so treat every language as a major component effort.

**Documentation setup**
- Create `docs/development/relation-tracking/<language>/` with the full 6-document suite (Spec, Design, Implementation Plan, Progress, Test Plan, Test Execution). Relation tracking spans core + plugin, so the abbreviated plugin process is insufficient.
- Reference existing Rust relation tracking docs when drafting the new SPEC to keep behaviour consistent (call edges, imports/exports, references, type metadata).

**Grammar research checklist** (capture findings in SPEC/DESIGN)
- Python: confirm `function_definition`, `call`, `import_statement`, and `import_from_statement` nodes in `tree-sitter-python` [`src/grammar.json`](https://github.com/tree-sitter/tree-sitter-python/blob/master/src/grammar.json). These underpin call/load relationships.
- JavaScript: map `function_declaration`, `call_expression`, `import_clause`, `export_statement`, and `named_exports` in `tree-sitter-javascript` [`src/grammar.json`](https://github.com/tree-sitter/tree-sitter-javascript/blob/master/src/grammar.json) to relation edges, including optional chaining call variants.
- TypeScript: reuse the JavaScript list and add `type_arguments` handling per `tree-sitter-typescript` [`typescript/src/grammar.json`](https://github.com/tree-sitter/tree-sitter-typescript/blob/master/typescript/src/grammar.json) so generic calls produce correct callee IDs.
- Go: cover `function_declaration`, `call_expression`, `method_spec`, `import_spec`, and selector expressions from `tree-sitter-go` [`src/grammar.json`](https://github.com/tree-sitter/tree-sitter-go/blob/master/src/grammar.json); note the special `new`/`make` call forms surfaced by the grammar.

**Implementation expectations**
- Extend each `sqry-lang-<language>` plugin’s tree-sitter query set to emit `CallEdge`, `ImportEdge`, `ExportEdge`, and `ReferenceEdge` payloads via the shared `RelationStore` API.
- Update query constants under `sqry-core/src/symbols/queries/` (or equivalent per-language module) to track: function/method definitions, method receivers, top-level exports, re-exports, import aliases, and call expressions (including async/await, optional chaining, decorators, chained selectors).
- Capture symbol resolution helpers (e.g., module path reconstruction for Python packages, TypeScript `import type`, Go selector expressions) inside core utilities rather than duplicating logic across plugins.
- Document any `RelationStore` schema adjustments in the DESIGN and bump `RELATION_STORE_VERSION` only when necessary, pairing the change with a migration plan.

**Testing requirements**
- Add language-specific fixtures under `tests/fixtures/<language>/relation_tracking/` covering: intra-file calls, cross-module imports, re-exports, async / generator usage, method calls on structs or classes, and error-handling constructs.
- Extend integration tests to assert both caller→callee and callee→caller lookups, import/export mapping, and reference counts using the new fixtures.
- Capture performance snapshots (target: <100ms for a 1000-line file) in `06_TEST_EXECUTION-_SLUG_.md`; if targets slip, create follow-up tasks with profiling notes.
- Verify degraded syntax (malformed files, partial parses) defers relation edges gracefully without panics—document fallback behaviour in `05_TEST_PLAN-_SLUG_.md`.

**Rollout & coordination**
- Sequence work to avoid large PRs: tackle one language at a time, landing SPEC/DESIGN/PLAN before implementation. Document dependencies (e.g., shared helpers) in each Implementation Plan.
- Update `docs/development/relation-tracking/<language>/04_PROGRESS-_SLUG_.md` after every working session with fixture coverage and outstanding risks; cross-link blockers shared between languages.
- After each language ships, update `README.md` support matrix and record version bumps (`feat(relations-<language>): ...`) with references to the relevant Implementation Plan steps.

---

## Semantic Versioning Automation

Release automation, commit-to-version mapping, manual fallbacks, and changelog conventions are documented centrally in `docs/SEMANTIC_VERSIONING.md`. Treat that file as the single source of truth when preparing releases or updating workflow references. This process guide only requires that:

1. All commits follow the conventional commit format.  
2. Each component’s Implementation Plan links to the expected bump (MINOR for `feat`, PATCH for `fix`/`perf`, etc.).  
3. Test execution, formatting, and release notes are complete before tagging.

---

## Error Handling

If implementation is requested without documents:

```
❌ Cannot proceed. This requires the sqry structured development process.

Based on the request, this is a: [major component / plugin / port from legacy upstream]

Process required:
- Major component: Full 6-document process
- Plugin: Simplified 3-document process
- Port from legacy upstream: Code-first + retrospective docs

Would you like me to create the documentation?
I'll start with `01_SPEC-_SLUG_.md` to define what and why.
```

If a feature doesn't pass the semantic search litmus test:

```
⚠️ Feature Review: Does this improve semantic code search?

The requested feature "<name>" doesn't clearly improve sqry's core mission.

Questions to consider:
1. Does this help users find code by semantic meaning?
2. Is this essential to the plugin architecture?
3. Could this be a separate tool/plugin instead?

See docs/CRITICAL_FEATURE_EVALUATION.md for our philosophy.

Please clarify how this serves semantic code search, or consider if this belongs elsewhere.
```

---

## Commit Strategy

### Standard Commits

```bash
# Documentation
docs(spec): add symbol extraction specification
docs(design): add plugin system design
docs(plan): add AST query implementation plan

# Implementation
feat(symbols): implement step 1 - basic symbol extraction
feat(symbols): implement step 2 - add caching layer
fix(cache): handle concurrent access properly

# Testing
test(symbols): add unit tests for Rust symbol extraction
docs(test-execution): symbol extraction tests passing

# Reviews
docs(review): add CODEX planning review for symbol extraction
docs(review): add CODEX code review for symbol extraction
```

### Porting Commits

```bash
# Porting from legacy upstream
port(search): port core search engine from legacy upstream
refactor(search): simplify search engine (remove streaming)
docs(port): document search engine porting decisions
```

### Version Bump Commits

```bash
# Automated by tooling, but manually if needed:
chore(release): bump version to 0.2.0
chore(changelog): update CHANGELOG for 0.2.0 release
```

---

## Success Criteria

### For Major Components
- ✅ Block implementation until Spec/Design/Plan are self-approved with documented rationale
- ✅ Update PROGRESS.md at least once per working session
- ✅ TEST_EXECUTION.md has actual results, not just a plan
- ✅ All tests pass before marking complete
- ✅ **Clippy Phase 1 (errors)**: All errors fixed, committed with implementation
- ✅ **Clippy Phase 2 (warnings)**: All warnings resolved, separate commit
- ✅ **Clippy Phase 3 (pedantic)**: All pedantic issues addressed, separate commit
- ✅ CODEX reviews completed (planning + code)
- ✅ HIGH priority AI recommendations addressed or exceptions documented
- ✅ All acceptance criteria verified
- ✅ Version bumped according to semver rules
- ✅ CHANGELOG.md updated (automatically by tooling)
- ✅ PR links complete document set

**Note**: Implementation is NOT complete until all 3 clippy phase commits are made.

### For Plugins
- ✅ 3-document set complete (Spec, Implementation, Tests)
- ✅ Tests pass for all supported language features
- ✅ Follows plugin template pattern
- ✅ PATCH version bump (plugin addition is enhancement)

### For Ports from legacy upstream
- ✅ Port plan documents mapping and changes
- ✅ Tests verify functional equivalence
- ✅ Retrospective docs explain architecture
- ✅ Feature bloat removed (verify against original)

---

## Example: Symbol Extraction System

**Request**: "Implement symbol extraction system"

**Process**:
1. Determined: Major component, requires full 6-doc process
2. Created `docs/development/ARCHIVE/symbol-extraction/` directory
3. Created `01_SPEC-_SLUG_.md`
   - Problem: Need to extract semantic symbols from code
   - Goal: AST-based symbol extraction for indexing
   - Semantic search test: ✅ Enables finding symbols by meaning
   - Self-approved with rationale
4. Created `02_DESIGN-_SLUG_.md`
   - Architecture: SymbolExtractor trait + per-language implementations
   - API signatures: `extract_symbols(&self, content: &[u8]) -> Result<Vec<Symbol>>`
   - Alternatives: Regex-based (rejected - not semantic), LSP (rejected - too complex)
   - Self-approved
5. Created `03_IMPLEMENTATION_PLAN-_SLUG_.md`
   - Step 1: Define Symbol struct (<50 LOC)
   - Step 2: Implement SymbolExtractor trait (<100 LOC)
   - Step 3: Add Rust language implementation (<150 LOC)
   - Self-approved
6. Created `05_TEST_PLAN-_SLUG_.md`
   - Unit tests for each symbol type
   - Integration tests with real Rust files
   - Edge cases: malformed code, Unicode, etc.
   - Target: >90% coverage
7. Ran CODEX + Claude Code planning review
   - HIGH: Add support for nested symbols (classes with methods)
   - MEDIUM: Consider incremental updates
   - LOW: Add performance benchmarks
   - Addressed HIGH, documented MEDIUM/LOW as future work
8. Implemented with `04_PROGRESS-_SLUG_.md` tracking
   - Commits: `feat(symbols): implement step 1`, `feat(symbols): implement step 2`, etc.
   - Version: 0.1.0 → 0.2.0 (minor bump for new feature)
9. Executed tests -> `06_TEST_EXECUTION-_SLUG_.md`
   - All tests pass
   - Coverage: 92%
   - All acceptance criteria verified
10. Ran CODEX + Claude Code code review
    - Result: 9.2/10 - APPROVED
    - HIGH: Add error handling for parse failures (fixed)
    - MEDIUM: Extract common code to helper (deferred to refactor)
11. Created PR with:
    - Links to all 6 docs + 2 review docs
    - Acceptance criteria checklist
    - Version bump confirmation (0.1.0 → 0.2.0)
    - CHANGELOG excerpt

**Outcome**: Feature complete with 92% test coverage, comprehensive reviews, full documentation, and proper versioning.

---

## Maintenance

### Review Process Annually
- Does the process still serve lean development?
- Are we creating unnecessary documentation?
- Is AI review providing value?

### Process Improvements
- Document pain points in `docs/PROCESS_IMPROVEMENTS.md`
- Discuss in quarterly retrospectives
- Update process based on learnings

### Tools
- Keep tooling minimal
- Automate repetitive tasks (version bumping, changelog)
- Avoid tool bloat (ironic meta-rule)

---

## Philosophy Reminder

This process exists to:
- ✅ **Maintain quality** without sacrificing speed
- ✅ **Enable solo development** with self-approval
- ✅ **Prevent feature creep** via semantic search test
- ✅ **Ensure traceability** for future maintenance
- ✅ **Leverage AI** for review and quality checks

This process should NOT:
- ❌ Create documentation for documentation's sake
- ❌ Slow down development unnecessarily
- ❌ Add complexity without clear value
- ❌ Become a bureaucratic hurdle

**When in doubt**: Ask "Does this serve semantic code search?" If yes, do it. If no, skip it.
