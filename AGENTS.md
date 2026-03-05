# AGENTS.md — sqry Repository Guide

## Scope

This file applies to the entire sqry repository subtree.
All AI agents (Claude Code, Codex, Gemini, etc.) must follow these rules for every file they touch within this repo.

---

## GROUND RULES (HIGHEST PRIORITY - READ FIRST)

### Deployment Focus

sqry is being built for **FULL PRODUCT DEPLOYMENT**. We are not pursuing:
- ❌ Proof-of-concept targets
- ❌ "Minimum viable product" shortcuts
- ❌ "MVP" approaches
- ❌ Phased rollouts ("Priority 1 now, rest later")
- ❌ "We'll add the polish later" thinking
- ❌ "Ship the core first" patterns

**Every task must deliver the COMPLETE, production-ready feature, software, implementation.**

### Mandatory Rules (NEVER VIOLATE)

1. **DEAL with COMPLEXITY** when it's found - DO NOT skip, mock, stub, shortcut, create placeholders or simplify
2. **QUALITY and SECURE** code and coding practice are foremost goals
3. **Stick to our defined patterns**, or research and create patterns we can refine
4. **NO TIME CONSTRAINTS** - thoroughness over speed, always
5. **NO TOKEN CONSTRAINTS** - provide complete solutions
6. **NO ACTIVE USERS** - we are in DEVELOPMENT, we DO NOT have to consider providing migration paths for changes
7. **DO NOT ASSUME** - validate code and documentation before you change anything
8. **Use current server date** for all timestamps in docs/reviews (avoid stale dates). Run `date -u +"%Y-%m-%d"` (and include time/zone if needed) from this server; do not rely on local/desktop time.

### Anti-Patterns (NEVER DO THESE)

- ❌ **"Priority 1 now, Priority 2/3 later"** - This is how work never gets finished
- ❌ **"Ship the core, iterate later"** - Iterations never happen, features stay incomplete
- ❌ **"Let's do a quick MVP"** - MVP means "minimal viable product" = incomplete = WRONG
- ❌ **"We can add [feature] in a follow-up"** - Follow-ups rarely happen
- ❌ **"This is good enough for now"** - Nothing is "good enough" until it's COMPLETE
- ❌ **Phased implementations** - Plan the whole thing, implement the whole thing, ship the whole thing
- ❌ **"TODO: implement later"** - Implement it NOW and implement the best possible version/solution

### What "Complete" Means

When implementing a feature (e.g., "CLI/VSCode Sync"):
- ✅ All components working (lock files + CLI + LSP + Extension + validation + monitoring)
- ✅ All error handling (not just happy path)
- ✅ All testing (unit + integration + edge cases)
- ✅ All documentation (not "we'll document later")
- ✅ All polish (error messages, logging, telemetry)
- ✅ Production-ready referece implementation quality

**What you deliver must be COMPLETE.**

---

## Mission

Build a **lean, focused semantic code search tool** in Rust (1.89+, Edition 2024) that understands code structure through AST analysis. Enable developers to search code by what it **means**, not just what it says.

**Core Philosophy**: Do one thing exceptionally well - semantic code search.

**Not in scope**: Linters, metrics platforms, IDEs, servers, or language-specific analyzers.

## REVIEWS:
1. There are other agents on this server. 
2. Consider the other agent as your pair programmer.
3. You MUST ask for reviews at each step in a process.  The review ask MUST be recorded in a document in persistent storage NOT tmp. The other agent MUST be instructed to provide a response in a <filename>_review.md document.
4. Both you AND the other agent must agree on the CORRECT way forward.  
5. Example, if another agent asks for or suggests a change, 
    a. implement it if you agree AND 
    b. verify with the other agent that the change met their requirements,
    c. if you don't agree, explain your reasoning when you respond to the other agent.

See: `IMPLEMENTATION_PLAN.md`, `docs/CRITICAL_FEATURE_EVALUATION.md`, and `README.md`.

---

## Quick Compliance Checklist

Use this before touching code or docs. Every item must be ✅ or you stop.

| Area | Question | Where to confirm |
| --- | --- | --- |
| **Branch** | **Are you on the correct git branch?** | **`git branch --show-current`** |
| **Location** | **Are you in the correct working directory?** | **`pwd` (should be ``)** |
| **Folder structure** | **Does the target directory exist before writing files?** | **`ls <parent-dir>/`** |
| Mission fit | Does the task clearly improve semantic code search? | `docs/CRITICAL_FEATURE_EVALUATION.md`, spec |
| Planning | Are SPEC/DESIGN/PLAN docs approved (or qualifies for exception)? | `docs/development/<component>/` |
| Docs-first | Is the user-facing guide/tutorial updated or drafted? | Referenced in 01_SPEC + 03_PLAN |
| Tests | Do you have a test plan + required fixtures identified? | `05_TEST_PLAN.md`, `tests/fixtures/` |
| Toolchain | Have you run `cargo fmt`, `cargo test`? | Local shell history / CI |
| **Clippy phases** | **Have you completed all 3 clippy phases (errors→warnings→pedantic)?** | **Section "Clippy Compliance"** |
| Reviews | Are Codex + Claude reviews scheduled or exemption logged? | `CODEX_REVIEW.md`, `CODEX_CODE_REVIEW.md` |
| Commit/Versioning | Do you know the correct Conventional Commit type + semver bump? | Section "Commits & Pull Requests" |
| Environment | Do sandbox/approval settings permit the commands you plan to run? | CLI banner / section "Environment & Approvals" |
| **File validation** | **After write/edit: did you verify content with Read tool?** | **Use `Read` tool to confirm changes** |

---

## Repository Layout

### Workspace Structure
```
sqry/
├── README.md                      # Project overview
├── AGENTS.md                      # This file
├── Cargo.toml                     # Workspace manifest
├── IMPLEMENTATION_PLAN.md         # Historical 9-week roadmap (see docs/ for current plans)
├── docs/
│   ├── DEVELOPMENT_PROCESS.md     # Development workflow
│   ├── CRITICAL_FEATURE_EVALUATION.md  # Design philosophy
│   ├── development/<component>/   # Per-component docs (incl. CONSOLIDATED/ARCHIVE_TECHNICAL_BACKLOG)
│   └── templates/                 # Document templates
├── sqry-core/                     # Core library
├── sqry-cli/                      # CLI binary (builds as 'sqry')
├── sqry-lsp/                      # LSP server (language server)
├── relations-shared/              # Shared relations engine for plugins
├── sqry-lang-*/                   # Language plugins (Rust, JS, TS, Python, Go, SQL, Terraform, Puppet, etc.)
├── tests/                         # Cross-crate integration tests
└── test-fixtures/                 # Shared test fixtures for plugins and CLI
```

### Future Crates (add as needed)
- `sqry-lang-*`: Additional language plugins (Ruby, Java, C#, etc.)
- Keep PRs small and focused

---

## Environment & Approvals

The CLI prints `sandbox_mode`, `network_access`, and `approval_policy` at session start. Act accordingly:

| Setting | Meaning | Required behavior |
| --- | --- | --- |
| `sandbox_mode=read-only` | File writes blocked | Request approval before any mutation; prefer planning/documentation work |
| `sandbox_mode=workspace-write` | Only repo + writable roots allowed | Keep edits inside repo; ask before touching other paths |
| `sandbox_mode=danger-full-access` | Full filesystem | Still respect “no destructive commands” rule; double-check paths |
| `network_access=restricted` | Outbound network blocked | Don’t curl/git clone without approval; lean on local docs |
| `network_access=enabled` | Network permitted | Still avoid unapproved telemetry or uploads |
| `approval_policy=never` | Cannot escalate commands | Find alternatives that stay within sandbox limits |
| `approval_policy=on-request` | You may escalate when justified | Provide one-sentence justification per command |
| `approval_policy=on-failure` | Retry with escalation only if sandboxed command fails | Capture failure output before re-running |

If a command is blocked, record the reason in your notes and either (a) request escalation (unless policy=never) or (b) revise the plan so work continues without it. Never assume prior approvals carry over to a new session.

---

## Toolchain & Builds

### Rust Version
- **Edition**: 2024
- **Rust version**: 1.89+ (minimum `rust-version = "1.89"`)
- **Prefer stable**: Avoid nightly-only features

### Commands
```bash
# Build
cargo build --workspace

# Tests (REQUIRED - must pass before any commit)
cargo test --workspace

# Format (REQUIRED before commit)
cargo fmt --all

# Build CLI binary
cargo build --bin sqry

# Run CLI
./target/debug/sqry --help
```

### Clippy Compliance (MANDATORY - Phased Commits)

Implementation is **NOT complete** until all three clippy phases pass. This is a **hard requirement**:

| Phase | Command | Requirement | Commit |
|-------|---------|-------------|--------|
| **1. Errors** | `cargo clippy --all-targets --workspace -- -D warnings` | All errors/problems MUST be fixed | First commit (with implementation) |
| **2. Warnings** | `cargo clippy --all-targets --workspace` | All warnings MUST be resolved | Separate commit after Phase 1 |
| **3. Pedantic** | `cargo clippy --workspace -- -W clippy::pedantic` | All pedantic issues MUST be addressed | Separate commit after Phase 2 |

**Workflow**:
```bash
# Phase 1: Fix all errors (blocks first commit)
cargo clippy --all-targets --workspace -- -D warnings
# Fix all issues, then commit implementation

# Phase 2: Fix all warnings (separate commit)
cargo clippy --all-targets --workspace
# Fix all warnings, commit: "chore(clippy): resolve warnings"

# Phase 3: Fix pedantic issues (separate commit)
cargo clippy --workspace -- -W clippy::pedantic
# Fix all issues, commit: "chore(clippy): resolve pedantic lints"
```

**Implementation is complete only after Phase 3 commit is made.**

### No Mixed Languages
- **Only Rust** for production code
- Shell scripts allowed only for:
  - `.github/` (CI/CD)
  - `scripts/` (development utilities)
- No Python, JavaScript, or other languages in core codebase

---

## Repository Knowledge Graph (RKG)

The Repository Knowledge Graph tracks relationships between requirements, code, tests, docs, and CI pipelines. It's enforced via CI and provides machine-readable traceability.

### Updating the RKG
After adding/removing specs, crates, tests, or docs:
```bash
# Regenerate graph
cargo run -p graphsync -- --full

# Check for drift (CI does this automatically)
cargo run -p graphsync -- --full --check
```

### Annotating Code
Add inline annotations to create custom edges:
```rust
// RKG: implements REQ:SQRY-P2-7-PARALLEL-QUERY-EXECUTION
// RKG: tests CODE:SQRY-CORE
```

Supported relations: `implements`, `tests`, `documents`, `validated_by`

### Manual Overrides
Create `graphsync.toml` in repo root for manual mappings:
```toml
[edges]
"REQ:SQRY-CUSTOM" = ["CODE:SQRY-CORE", "CODE:SQRY-CLI"]

[tests]
"CODE:SQRY-CORE" = ["TEST:CORE-CUSTOM"]
```

### CI Enforcement
The RKG check runs on every CI build (Ubuntu). If it fails:
1. Run `cargo run -p graphsync -- --full` locally
2. Review the diff in `.verivus/graph/rkg.json`
3. Commit the updated graph

See `docs/development/graphsync/` for full spec/design/plan.

---

## Coding Style

### Naming Conventions
- **Modules/functions/variables**: `snake_case`
- **Types (structs/enums)**: `PascalCase`
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Traits**: `PascalCase` (e.g., `LanguagePlugin`)

### Error Handling
- **Applications** (CLI): Use `anyhow::Result<T>`
- **Libraries** (core, plugins): Use `thiserror` for custom errors
- Always return `Result<T, E>`, never panic in library code
- Panic only for invariant violations in internal code

### Logging
- Use `log` crate with `env_logger`
- **Libraries**: Use `log::debug!()`, `log::info!()`, etc.
- **CLI**: Can use `println!()` for user-facing output
- No `println!()` in library code

### Memory & Performance
- Prefer `Arc<str>` and string interning for symbol-heavy data
- Use `&str` when possible, avoid `String` allocations
- Profile before optimizing (don't prematurely optimize)

### Concurrency
- Use `rayon` for CPU-bound parallelism (data processing)
- Use `parking_lot` for faster locks (Mutex, RwLock)
- Avoid `tokio` unless IO-bound concurrency is required
- Prefer thread-safe types: `Arc`, `DashMap`, `parking_lot::Mutex`

### Serialization
- **Serde** for all serialization
- **Format preferences**:
  - **Binary**: `bincode` for index files (fast, compact)
  - **Human-readable**: `serde_json` for CLI output, debugging
- Include version fields in serialized structures for future compatibility

### Documentation
- Public APIs require doc comments (`///`)
- Examples in doc comments for non-trivial functions
- Module-level docs (`//!`) for each major module
- Link to related types/functions in docs

### Code Quality & Linting

#### Clippy Pedantic Lints
sqry targets clippy pedantic compliance to maintain high code quality:

```bash
# Check pedantic lints (advisory, not blocking)
cargo clippy --workspace -- -W clippy::pedantic

# Generate pedantic analysis
cargo clippy --workspace -- -W clippy::pedantic 2>&1 | tee clippy-pedantic.log
```

**Handling pedantic warnings**:
1. **Auto-fix when safe**: `cargo clippy --fix --allow-dirty -- -W clippy::pedantic`
2. **Manual fixes for semantic issues**: Address `similar_names`, `module_inception`, etc. by improving code clarity
3. **Document suppressions**: Use `#[allow(clippy::...)]` with inline comments explaining why
4. **Track progress**: Create analysis documents in `docs/development/` for systematic cleanup

**Common patterns**:
- `similar_names`: Rename variables for semantic clarity (e.g., `context` → `semantic_context` when `content` is also in scope)
- `module_inception`: Rename module or type to avoid `module::Module` pattern
- `missing_errors_doc`: Add `# Errors` sections to fallible functions
- `missing_panics_doc`: Add `# Panics` sections to functions that may panic
- `ignored_unit_patterns`: Match on `()` explicitly or use `#[allow]` with reason
- `must_use_candidate`: Add `#[must_use]` to functions whose return values shouldn't be ignored
- `unwrap`/`expect`/`panic!`: Production enforcement is via clippy/pedantic; slopscan surfaces these as low-severity informational findings by default.

**Distinction from naming conventions**:
- **Clippy similar_names**: Confusingly similar variables in same scope (e.g., `matcher`/`matches`)
- These are separate concerns: completing naming conventions doesn't eliminate similar_names warnings

#### Test Infrastructure Patterns

**Binary path resolution in tests**:
- Never access `CARGO_BIN_EXE_*` environment variables directly
- Use common test helper modules with fallback paths
- Pattern works in both CI (env var set) and local workspace contexts (fallback to target/ directory)

Example (sqry-cli/tests/common/mod.rs):
```rust
pub fn sqry_bin() -> PathBuf {
    std::env::var("CARGO_BIN_EXE_sqry")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let manifest_dir = env!("CARGO_MANIFEST_DIR");
            let workspace_dir = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();
            let debug_path = workspace_dir.join("target/debug/sqry");
            let release_path = workspace_dir.join("target/release/sqry");
            if debug_path.exists() {
                debug_path
            } else if release_path.exists() {
                release_path
            } else {
                panic!("Could not find sqry binary...");
            }
        })
}
```

**Test isolation with serial_test**:
- Use `serial_test` crate for tests that modify global state (environment variables, filesystem, etc.)
- Add `serial_test = "3.0"` to dev-dependencies
- Mark tests with `#[serial]` attribute to prevent parallel execution interference

Example:
```rust
use serial_test::serial;

#[test]
#[serial]
fn test_env_override() {
    unsafe {
        std::env::set_var("MY_VAR", "value");
    }
    assert_eq!(my_function(), expected);
    unsafe {
        std::env::remove_var("MY_VAR");
    }
}
```

**Common test helper functions**:
- Keep complete helper modules - don't partially implement
- Mark shared utilities with `#[allow(dead_code)]` to prevent false warnings
- Include: fixture paths, test server setup, index building, binary location

**Test ignore reasons**:
- All `#[ignore]` attributes must include reasons
- Format: `#[ignore = "reason"]`
- Common reasons:
  - "Integration test - run in nightly job to keep CI fast"
  - "Performance test - run in nightly job to keep CI fast"
  - "Expensive rebuild test - enable for validation testing"
  - "Requires external service - run manually when service available"

---

## Tree-sitter Integration

### Language Grammars
- Use official tree-sitter grammars: `tree-sitter-rust`, `tree-sitter-javascript`, etc.
- Pin to stable versions (e.g., `0.23.x`)
- Update cautiously (test thoroughly after updates)

### Symbol Extraction Strategy
- **Prefer tree-sitter queries** over manual AST traversal
- Define queries in separate files or constants (not inline)
- See `IMPLEMENTATION_PLAN.md` for query-based approach
- Cache parsed trees (AST) in LRU cache

### Query Examples
```rust
// Good: Query-based extraction
const RUST_FUNCTION_QUERY: &str = r#"
    (function_item
        name: (identifier) @func.name
        parameters: (parameters) @func.params
    ) @func
"#;

// Avoid: Manual traversal (only when queries insufficient)
fn extract_symbols_manual(node: Node) -> Vec<Symbol> {
    if node.kind() == "function_item" { /* ... */ }
}
```

---

## Plugin System Architecture

### Plugin Trait
All language plugins must implement `LanguagePlugin` trait (defined in `sqry-core/src/plugin/`):

```rust
pub trait LanguagePlugin: Send + Sync {
    fn metadata(&self) -> LanguageMetadata;

    /// File extensions this plugin supports (e.g. "rs", "js")
    fn extensions(&self) -> &'static [&'static str];

    /// Extract symbols from raw source bytes
    fn extract_symbols(
        &self,
        content: &[u8],
        file: &Path,
    ) -> Result<Vec<Symbol>, SymbolExtractionError>;

    /// Extract symbols from a pre-parsed AST tree
    fn extract_symbols_from_tree(
        &self,
        tree: &Tree,
        content: &[u8],
        file: &Path,
    ) -> Result<Vec<Symbol>, SymbolExtractionError>;

    /// Parse AST for this language
    fn parse_ast(&self, content: &[u8]) -> Result<Tree, ParseError>;

    /// Tree-sitter query source for symbol extraction
    fn symbol_query(&self) -> &'static str;

    /// Extract scope information for context-aware search (P2-34)
    fn extract_scopes(
        &self,
        tree: &Tree,
        content: &[u8],
        file_path: &Path,
    ) -> Result<Vec<Scope>, ScopeError>;

    /// Optional cross-file symbol resolution (P2-33+)
    fn resolve_symbol(&self, symbol: &str, context: &Path) -> PluginResult<Option<SymbolRef>>;

    // ... other methods (relations, field descriptors, etc.)
}
```

### Plugin Development Guidelines
- Built-in plugins: `sqry-lang-{rust,javascript,typescript,python,go}`
- External plugins: Separate repos (e.g., `sqry-lang-ruby`)
- Use simplified 3-doc process for plugins (see `DEVELOPMENT_PROCESS.md`)
- Follow template in `docs/templates/PLUGIN_TEMPLATE.md`

### Plugin Testing
- Test each plugin with real-world code samples
- Include edge cases (Unicode, malformed code, fuzzing, etc.)
- Benchmark symbol extraction performance
- Target: <100ms for 1000-line files

---

## Core Architecture Principles

### Three-Layer Design
```
┌─────────────────────────────┐
│   CLI Layer (sqry-cli)      │  ← User interaction, argument parsing
└─────────────────────────────┘
            │
            ▼
┌─────────────────────────────┐
│   Core Engine (sqry-core)   │  ← Search, symbols, AST, cache, plugin system
└─────────────────────────────┘
            │
            ▼
┌─────────────────────────────┐
│ Language Plugins (sqry-lang)│  ← Rust, JS, TS, Python, Go, ...
└─────────────────────────────┘
```

### Module Organization
**sqry-core** modules:
- `search`: Search engine and pattern matching
- `symbols`: Symbol extraction and indexing
- `ast`: AST querying and query language
- `cache`: LRU cache with optional persistence
- `plugin`: Plugin system traits and management
- `output`: Output formatters (text, JSON)

### Data Structures (Key Types)
```rust
// Symbol representation
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub location: Location,
    pub signature: Option<String>,
    // ...
}

pub enum SymbolKind {
    Function,
    Class,
    Method,
    Variable,
    // ...
}

// Index structure
pub struct SymbolIndex {
    symbols: Vec<Symbol>,
    by_name: HashMap<String, Vec<usize>>,
    by_file: HashMap<PathBuf, Vec<usize>>,
    // ...
}
```

---

## Testing Requirements

### Test Coverage
- **Target**: >80% coverage for core, 100% for critical paths
- **Required**: Tests must pass before any commit (`cargo test --workspace`, `cargo test --warning`, `cargo clippy -- -W clippy::pedantic` )
- **Framework**: Built-in Rust test framework (`#[test]`, `#[cfg(test)]`)
- **Infrastructure patterns**: See [Test Infrastructure Patterns](#test-infrastructure-patterns) for binary path resolution, test isolation, and helper functions

### Test Organization
```rust
// Unit tests: In same file as code
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_extraction() {
        // ...
    }
}

// Integration tests: In tests/ directory
// tests/symbol_extraction.rs
#[test]
fn test_end_to_end_symbol_extraction() {
    // ...
}
```

### Test Types
1. **Unit tests**: Test individual functions/methods
2. **Integration tests**: Test component interaction
3. **End-to-end tests**: Test CLI with real files
4. **Edge case tests**: Unicode, malformed input, empty files, etc.
5. **Performance tests**: Benchmark critical paths (optional)

### Test Data
- Use `tempfile` crate for test isolation
- Include real-world code samples in `tests/fixtures/`
- Test against popular open-source projects (Rust, JS, etc.)

### Coverage Tooling
- Preferred command: `cargo llvm-cov --workspace --html` (requires `cargo-llvm-cov`); publish the HTML path in `06_TEST_EXECUTION.md`.
- Lightweight fallback: `cargo tarpaulin --workspace --out Html` for Linux-only CI jobs.
- If neither tool is available, document the gap in `05_TEST_PLAN.md` and provide manual evidence (e.g., per-crate coverage numbers) before merging.

---

## Performance Guidelines

### Targets (from IMPLEMENTATION_PLAN.md)
- **Time to first result**: <100ms (90th percentile)
- **Index build**: <10s per 1000 files
- **Incremental rebuild**: <1s
- **Memory footprint**: <100MB (typical project)
- **Binary size**: <20MB (stripped release)

### Optimization Strategy
1. **Profile first**: Use `cargo flamegraph` or `perf`
2. **Measure**: Benchmark before/after changes
3. **Optimize hot paths**: Focus on 80/20 rule
4. **Incremental indexing**: Only re-index changed files
5. **Mmap large files**: Use `memmap2` for >1MB files
6. **LRU caching**: Cache parsed ASTs (limit: 100 entries default)

### No Premature Optimization
- Focus on correctness first
- Optimize when measurements show need
- Document performance assumptions

---

## Security & Privacy

### Security Principles
- **No secrets in commits**: Use `.gitignore`, check before commit
- **Respect .gitignore**: Honor user's ignore patterns
- **Local-first**: All processing happens locally
- **No telemetry**: Zero data collection or phone-home
- **No network calls**: Except for plugin downloads (future)

### Input Validation
- Sanitize file paths (prevent path traversal)
- Handle malformed source files gracefully
- Limit recursion depth (prevent stack overflow)
- Validate tree-sitter queries before execution

### Ongoing Security Practices (MUST)
- **Automated auditing**: Integrate `cargo-audit` into CI/CD so dependency CVEs are caught immediately.
  ```bash
  cargo install cargo-audit
  cargo audit
  ```
- **Dependency scanning**: Run `cargo-deny` as part of the pipeline to detect license, vulnerability, and duplicate issues.
  ```bash
  cargo install cargo-deny
  cargo deny check
  ```
- **Regular updates**: Schedule quarterly dependency reviews. Execute `cargo update` in a staging branch, review upstream changelogs for breaking changes, and run the full test suite before merging.
- **Lock file management**: Keep `Cargo.lock` committed and up to date to guarantee reproducible builds across environments.

---

## Structured Development Process (Mandatory)

- Canonical process guide: `docs/DEVELOPMENT_PROCESS.md` (full 6-doc workflow).
- Quick checklist: `docs/DEVELOPMENT_PROCESS_CHECKLIST.md` (one-page reference).

### Process Selection Matrix

| Change type | Docs required | Reviews required | Notes |
| --- | --- | --- | --- |
| Docs-only / tests-only / chore | Update affected docs/tests; no spec refresh needed | Skip AI reviews (per exemption list) | Still run fmt/test if code touched |
| Bug fix ≤50 LOC, no API change | 04_PROGRESS + 06_TEST_EXECUTION entry; optional retrospective summary | Optional (Codex/Claude) | Document why 6-doc pack not needed |
| Feature/refactor >50 LOC or API-impacting | Full 6-doc pack (01–06) before coding | Both Codex + Claude (planning + code) | Ensure CLI template included if user-facing |
| Language plugin work | 3-doc plugin pack (SPEC/IMPLEMENTATION/TESTS) | Planning + code reviews unless pure docs/tests | Use plugin template |
| Emergency hotfix | Minimal docs upfront (impact note + rollback plan), but backfill 6-doc pack within 24h | Get at least one AI review post-fix | Only for production outages |

### Core Rule

### When Required (6-Document Process)
For new components, major refactors (>100 LOC), API changes, breaking changes:

1. `docs/development/<component>/01_SPEC.md` (🔒 What & Why)
2. `docs/development/<component>/02_DESIGN.md` (🔒 How)
3. `docs/development/<component>/03_IMPLEMENTATION_PLAN.md` (🔒 Steps)
4. `docs/development/<component>/04_PROGRESS.md` (🔄 Live status)
5. `docs/development/<component>/05_TEST_PLAN.md` (🔒 Verification)
6. `docs/development/<component>/06_TEST_EXECUTION.md` (🔄 Live results)

### When Simplified (3-Document Process)
For language plugins:
1. `docs/development/plugins/<lang>/01_SPEC.md`
2. `docs/development/plugins/<lang>/02_IMPLEMENTATION.md`
3. `docs/development/plugins/<lang>/03_TESTS.md`

### When Retrospective (Code-First)
1. Port the code first (logic already proven)
2. Write retrospective docs after:
   - `PORT_PLAN.md`: What was ported, how adapted
   - `01_SPEC.md`: What component does (retrospective)
   - `02_DESIGN.md`: Architecture as implemented
   - `TEST_PLAN.md` + `TEST_EXECUTION.md`: Verification

### Templates
Use templates from `docs/templates/`:
- `01_SPEC.md`, `02_DESIGN.md`, `03_IMPLEMENTATION_PLAN.md`, etc.
- `PLUGIN_TEMPLATE.md` for language plugins
- `CLI_INTEGRATION_TEMPLATE.md` for any CLI-facing feature (copy into planning pack before design approval)

### CLI Planning Requirements
- Populate the CLI integration template and keep it anchored in the component directory (`CLI_INTEGRATION.md`) for every command/flag/output change.
- Planning/AI reviews must confirm the template is present and consistent with Spec/Design/Plan docs.

### Docs-First Rule (Guides as Tests)
- Draft or update the relevant user guide, tutorial, or CLI help **before writing code**.
- Reference the planned guide in 01_SPEC.md and 03_IMPLEMENTATION_PLAN.md.
- Record completion of the guide update in 04_PROGRESS.md prior to implementation.
- Failure to prep documentation blocks implementation and review sign-off.

### Self-Approval Process
Since this is solo development (Phase 0-1):
1. Write Spec/Design/Plan
2. Self-review with documented rationale
3. Mark as approved: `🔒 Read-Only (Approved: YYYY-MM-DD by: Name - Rationale: reason)`
4. Proceed to implementation

**Example**:
```markdown
🔒 Read-Only (Approved: 2025-10-01 by: Werner Kasselman)
Rationale: Symbol extraction is core to Phase 1 (Weeks 2-3) and essential for
AST-based search. Passes semantic search test: enables finding symbols by
semantic meaning, not text patterns.
```

### Semantic Search Litmus Test
Every feature must answer: **"Does this make sqry better at semantic code search?"**

If no, reject the feature. See `docs/CRITICAL_FEATURE_EVALUATION.md`.

---

## AI Planning & Code Review (CODEX + Gemini + Claude Code)

Use multi-agent reviews to keep planning and implementation honest. The canonical workflow lives in `docs/DEVELOPMENT_PROCESS.md` (see sections **8. AI Planning Review** and **11. AI Code Review**); follow that end to end. Quick reminders:

- Run Codex, Gemini, and Claude Code reviews once planning docs are self-approved, and again after implementation/testing is green.
- Archive artifacts under `docs/reviews/<component>/<YYYY-MM-DD>/` (commands, summaries, extra scripts) and reference them from the component's `CODEX_REVIEW.md` / `CODEX_CODE_REVIEW.md`.
- Treat HIGH and MEDIUM/LOW findings as blocking; document items before proceeding.
- Use the command snippets in the process guide as examples—update them as the CLI evolves, but keep the intent (comprehensive, cited feedback).

### Gemini CLI Prerequisites

**IMPORTANT**: Gemini CLI requires specific configuration to write review files automatically.

**Required Configuration** (`~/.gemini/settings.json`):
```json
{
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

**Review Script Configuration**:
- The `request_review_with_uuid.sh` script uses `--approval-mode=yolo` flag for Gemini
- This ensures WriteFileTool is automatically approved without user interaction
- Gemini's sandbox restricts writes to workspace directory only (security feature)

**Troubleshooting**:
- If review files are empty (0 bytes), check WriteFileTool is in settings.json
- Verify Gemini CLI version >= 0.17.0 (has WriteFileTool support)

**Backup Before Modifying**:
```bash
cp ~/.gemini/settings.json ~/.gemini/settings.json.backup.$(date +%Y%m%d_%H%M%S)
```

### When to Skip AI Review
- Bug fixes <50 LOC
- Documentation updates
- Test additions (not test infrastructure changes)
- Chores (dependency updates, formatting)

---

## Commits & Pull Requests

### Conventional Commits (REQUIRED)

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>[optional scope]: <description>

[optional body]

[optional footer(s)]
```

**Types** (determines semantic versioning):
- `feat`: New feature → **MINOR bump** (0.1.0 → 0.2.0)
- `fix`: Bug fix → **PATCH bump** (0.2.0 → 0.2.1)
- `docs`: Documentation only → no bump
- `test`: Tests only → no bump
- `refactor`: Code refactoring → no bump (unless BREAKING CHANGE)
- `perf`: Performance improvement → **PATCH bump**
- `chore`: Maintenance tasks → no bump
- `ci`: CI/CD changes → no bump

**Breaking changes**: Add `BREAKING CHANGE:` in footer → **MAJOR bump** (0.2.1 → 1.0.0)

**Examples**:
```bash
# Feature (minor bump)
git commit -m "feat(symbols): add cross-file symbol resolution

Enables finding symbol definitions across multiple files.
Implements Phase 2 requirement for call graph analysis.

See: docs/development/symbol-extraction/03_IMPLEMENTATION_PLAN.md#step-5"

# Fix (patch bump)
git commit -m "fix(cache): prevent corruption on concurrent access

Race condition in LRU cache could cause index corruption.
Added proper locking with parking_lot::Mutex."

# Breaking change (major bump - avoid in 0.x.x)
git commit -m "feat(api)!: redesign LanguagePlugin trait

BREAKING CHANGE: LanguagePlugin trait method signatures changed.
Plugins must implement new extract_symbols_async() method.
Migrate by adding async wrapper around extract_symbols()."

# Non-versioned
git commit -m "docs: update README with plugin examples"
git commit -m "test: add edge case tests for AST query parser"
```

### Commit Message Guidelines
- **First line**: <50 chars, imperative mood ("Add", not "Added")
- **Body**: Explain what/why, not how (code shows how)
- **Reference docs**: Link to implementation plan steps
- **Breaking changes**: Explain migration path

### Pull Request Template

```markdown
## Component: <name>

### Documentation
- [ ] Spec: `docs/development/<component>/01_SPEC.md`
- [ ] Design: `docs/development/<component>/02_DESIGN.md`
- [ ] Implementation Plan: `docs/development/<component>/03_IMPLEMENTATION_PLAN.md`
- [ ] Progress: `docs/development/<component>/04_PROGRESS.md`
- [ ] Test Plan: `docs/development/<component>/05_TEST_PLAN.md`
- [ ] Test Execution: `docs/development/<component>/06_TEST_EXECUTION.md`
- [ ] CODEX Planning Review: `docs/development/<component>/CODEX_REVIEW.md`
- [ ] CODEX Code Review: `docs/development/<component>/CODEX_CODE_REVIEW.md`

### Acceptance Criteria
- [ ] Criterion 1 from SPEC (verified in: test X)
- [ ] Criterion 2 from SPEC (verified in: test Y)
- [ ] All tests passing (`cargo test --workspace`)
- [ ] Code formatted (`cargo fmt --all`)
- [ ] Clippy warnings addressed

### Semantic Versioning
- **Previous version**: 0.1.0
- **New version**: 0.2.0
- **Bump type**: Minor (new feature)
- **Commit type**: `feat(symbols): ...`

### Test Results
✅ All tests passing
📊 Coverage: 85% (target: >80%)
See: `06_TEST_EXECUTION.md` for details

### AI Review Summary
- **Planning review**: 8/10 - addressed all HIGH priority items
- **Code review**: 9/10 - minor suggestions documented for future
- See: `CODEX_REVIEW.md` and `CODEX_CODE_REVIEW.md`
```

### Small, Focused PRs
- One component/feature per PR
- <500 LOC changes when possible
- Multiple small PRs > one giant PR
- Easier to review, test, and merge

---

## Semantic Versioning

sqry applies conventional commits to drive automated versioning. Refer to `docs/SEMANTIC_VERSIONING.md` for the canonical mapping between commit types and version bumps, automation setup, manual fallback commands, and changelog expectations. Keep PR bodies focused on “what/why” so the release tooling can generate accurate notes.

---

## Agent Collaboration Rules

### Planning
- Use `TodoWrite` for multi-step work
- **Exactly one task** `in_progress` at a time
- Update todos frequently (after each step)

### Communication
- **Preambles**: Before tool calls, add 1-2 sentence explanation
- **File references**: Use clickable paths `src/lib.rs:42`
- **Be concise**: Minimize output tokens while maintaining clarity

### File Operations
- **Editing**: Use `Edit` tool (preferred) or `apply_patch`
- **Reading**: Use `Read` tool in ≤250-line chunks
- **Searching**: Use `Grep` for content search, `Glob` for file patterns
- **Minimal diffs**: Change only what's necessary

### File Operation Safety Checks (MANDATORY)
Before ANY file write or edit operation:
1. **Verify git branch**: Run `git branch --show-current` to confirm you're on the correct branch
2. **Verify folder structure**: Use `ls` to confirm parent directories exist before creating files
3. **Validate file writes**: After writing/editing, use `Read` tool to verify content was written correctly
4. **Check working directory**: Ensure you're in `` before operations

**Example workflow**:
```bash
# 1. Verify branch
git branch --show-current

# 2. Check folder exists before writing
ls docs/development/<component>/

# 3. Write/edit file
# ... use Write or Edit tool ...

# 4. Validate the write
# ... use Read tool to verify contents ...
```

**Never**:
- Write files to non-existent directories without creating them first
- Assume you're on the correct branch without verification
- Skip validation of file writes

### Validation
- **Before first commit**: Run `cargo build --workspace && cargo test --workspace && cargo clippy --all-targets --workspace -- -D warnings`
- **After first commit**: Complete clippy warnings fix, then commit separately
- **After warnings commit**: Complete clippy pedantic fix, then commit separately
- **Before PR**: Run `cargo fmt --all` (all clippy phases should already be committed)
- **Test failures**: Fix before proceeding, don't bypass

**Remember**: Implementation is complete only after the pedantic commit. See "Clippy Compliance" section.

### Scope Discipline
- **No drive-by refactors**: Don't change unrelated files
- **No feature creep**: Stick to the spec/plan
- **No premature optimization**: Profile first
- **No license headers**: Unless specifically requested

---

## Out of Scope (Until Explicitly Requested)

### Not Now (Maybe Later)
- Language server protocol (LSP) support
- IDE integrations (VS Code extension, etc.) - Phase 7
- Cross-repository search
- Web interface / server mode
- Metrics exporters (explicitly rejected - see CRITICAL_FEATURE_EVALUATION.md)
- Language-specific linters (C# LINQ, TypeScript smells, etc.)
- Design pattern detection
- File watching / interactive mode (maybe later as separate tool)

### Never (Against Philosophy)
- Metrics/monitoring platforms
- Linting functionality
- Code quality analysis (belongs in dedicated tools)
- Anything that doesn't serve semantic code search

---

## Repository Migration

**Current workspace location**: ``
**Previous local dev locations** (historical): `/home/werner/ADE/sqry`, `/home/werner/verivus-labs/sqry`

Migration to the current workspace is complete; treat `` as canonical for all agent work.

---

## References

- **Development Process**: `docs/DEVELOPMENT_PROCESS.md`
- **Implementation Plan**: `IMPLEMENTATION_PLAN.md` (9-week roadmap)
- **Philosophy**: `docs/CRITICAL_FEATURE_EVALUATION.md`
- **Technical Backlog (active)**: `docs/development/CONSOLIDATED_TECHNICAL_BACKLOG.md`
- **Technical Backlog (archive)**: `docs/development/ARCHIVE_TECHNICAL_BACKLOG.md`
- **Hard Limits Inventory**: `docs/HARD_LIMIT_INVENTORY.md`
- **Templates**: `docs/templates/`
- **README**: `README.md`

---

## How to Use This AGENTS.md

### For AI Agents
1. **Read this file first** before any task in sqry repo
2. **Follow all rules** (coding style, testing, commits, process)
3. **When in conflict**: Task instructions > AGENTS.md > defaults
4. **Ask questions**: If anything is ambiguous, ask before proceeding

### For Humans
1. This is the **authoritative guide** for agent behavior
2. Update this file as processes evolve
3. Keep it in sync with `DEVELOPMENT_PROCESS.md`
4. Review quarterly for improvements

---

## Error Handling for Agents

### If Asked to Implement Without Docs
```
❌ Cannot proceed. This requires the sqry structured development process.


Required process:
- Major component: Full 6-document process (see DEVELOPMENT_PROCESS.md)
- Plugin: Simplified 3-document process

Would you like me to create the documentation?
I'll start with 01_SPEC.md to define what and why.
```

### If Feature Doesn't Pass Litmus Test
```
⚠️  Feature Review: Does this improve semantic code search?

The requested feature "<name>" doesn't clearly serve sqry's core mission.

Questions to consider:
1. Does this help users find code by semantic meaning?
2. Is this essential to the plugin architecture?
3. Could this be a separate tool/plugin?

See: docs/CRITICAL_FEATURE_EVALUATION.md

Please clarify how this serves semantic code search, or consider if this
should be built as a separate tool.
```

### If Tests Fail
```
⚠️  Tests must pass before proceeding.

Failed tests:
- test_symbol_extraction: assertion failed
- test_ast_query_parse: panic at line 42

Action required:
1. Fix the failing tests
2. Run `cargo test --workspace` to verify
3. Update 06_TEST_EXECUTION.md with results
4. Then we can proceed to next step

Do not bypass failing tests.
```

---

## Success Criteria (For Agents)

When completing a task, verify:
- ✅ All required documents exist and are up-to-date
- ✅ Tests pass (`cargo test --workspace`)
- ✅ Code formatted (`cargo fmt --all`)
- ✅ Clippy clean (or warnings documented)
- ✅ Conventional commits used
- ✅ Version bumped if needed (feat/fix commits)
- ✅ CHANGELOG.md updated (if automated)
- ✅ AI reviews completed (planning + code)
- ✅ HIGH priority recommendations addressed
- ✅ Semantic search litmus test passes
- ✅ Documentation links working
- ✅ No feature creep introduced

---

## Philosophy (Final Reminder)

sqry exists to be **the best local semantic code search tool**.

Everything we build serves that mission. If it doesn't, we reject it.

**Quality over quantity. Focus over features. Lean over bloated.**

When in doubt: **Does this make semantic search better?** If yes, do it. If no, don't.

---

End of AGENTS.md
