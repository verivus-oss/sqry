# sqry Repository Guide

## Mission
Build a **lean, focused semantic code search tool** in Rust (1.90+, Edition 2024) that understands code structure through AST analysis. Enable developers to search code by what it **means**, not just what it says.

**Core Philosophy**: Do one thing exceptionally well - semantic code search.
**Not in scope**: Linters, metrics platforms, IDEs.

---

## Ground Rules (HIGHEST PRIORITY)

### Mandatory Rules
1. **DEAL with COMPLEXITY** - DO NOT skip, mock, stub, or simplify
2. **QUALITY and SECURE** code foremost
3. **Stick to defined patterns**
4. **NO TIME/TOKEN CONSTRAINTS** - thoroughness over speed
5. **DO NOT ASSUME** - validate before changing

### Anti-Patterns (NEVER DO)
- "Priority 1 now, rest later" / "Ship core, iterate later"
- "MVP" / "good enough for now" / "TODO: implement later"
- Phased implementations

### What "Complete" Means
- All components working + all error handling + all testing + all documentation + production-ready quality

---

## Quick Compliance Checklist

| Area | Check |
|------|-------|
| **Branch** | `git branch --show-current` |
| **Location** | Verify `pwd` matches expected repository root |
| **Folder** | `ls <parent-dir>/` before writing |
| Mission fit | Does task improve semantic code search? |
| Toolchain | `cargo fmt`, `cargo clippy`, `cargo test` |
| **File validation** | Read tool after write/edit |

---

## Repository Layout

```
sqry/
├── sqry-core/              # Core library (graph, symbols, search, plugin system)
├── sqry-cli/               # CLI binary ('sqry')
├── sqry-lsp/               # LSP server
├── sqry-mcp/               # MCP server (33 JSON-RPC tools for AI assistants)
├── sqry-nl/                # Natural language query translation
├── sqry-plugin-registry/   # Plugin registration and discovery
├── sqry-lang-*/            # 35 language plugins
├── sqry-mcp-redaction/     # MCP response redaction
├── sqry-test-support/      # Shared test infrastructure
├── sqry-tree-sitter-support/ # Tree-sitter helpers
├── docs/development/       # Per-component docs
└── test-fixtures/          # Shared test fixtures
```

---

## Toolchain

**Rust**: Edition 2024, version 1.90+

```bash
cargo build --workspace          # Build
cargo test --workspace           # Tests (REQUIRED before commit)
cargo fmt --all                  # Format (REQUIRED)
cargo clippy --all-targets -- -D warnings  # Clippy
```

---

## Unified Graph Architecture (v2.0.0+)

**Status**: ONLY implementation since v2.0.0 (Dec 2025). Legacy hooks removed (~13,200 LOC deleted). All 35 plugins use GraphBuilder.

### Core Design
- **Arena + CSR storage**: O(1) traversal, cache-friendly
- **Generational indices**: `NodeId` detects stale references
- **MVCC concurrency**: Single-writer, multi-reader with epoch snapshots
- **String interning**: Saves 10-50 MB

### Key Types

**NodeKind** (28 variants): Function, Method, Class, Interface, Trait, Module, Variable, Constant, Type, Struct, Enum, EnumVariant, Macro, Parameter, Property, CallSite, Import, Export, StyleRule, StyleAtRule, StyleVariable, Lifetime, Component, Service, Resource, Endpoint, Test, Other

**EdgeKind** (20+ variants with metadata):
- Structural: `Defines`, `Contains`
- References: `Calls{argument_count, is_async}`, `References`, `Imports{alias, is_wildcard}`, `Exports{kind, alias}`, `TypeOf`
- OOP: `Inherits`, `Implements`
- Cross-language: `FfiCall`, `HttpRequest`, `GrpcCall`, `WebAssemblyCall`, `DbQuery`
- Extended: `MessageQueue`, `WebSocket`, `GraphQLOperation`, `ProcessExec`, `FileIpc`, `ProtocolCall`

**Handle Types**: `NodeId{index, generation}`, `EdgeId`, `StringId`, `FileId`

### Concurrency
- **CodeGraph**: Arc-wrapped, O(1) snapshot creation
- **ConcurrentCodeGraph**: RwLock with MVCC
- **GraphSnapshot**: Immutable for queries

### Build Pipeline (5-Pass)
1. AST → Nodes (Defines/Contains edges)
2. Enrichment (visibility, types)
3. Intra-file edges (calls within file)
4. Cross-file (import resolution via ExportMap)
5. Cross-language (FFI linking, HTTP route matching, SQL detection)

### GraphBuilder Trait
```rust
pub trait GraphBuilder: Send + Sync {
    fn build_graph(&self, tree: &Tree, content: &[u8], file: &Path, staging: &mut StagingGraph) -> GraphResult<()>;
}
```

### Persistence
- Location: `.sqry/graph/snapshot.sqry`
- CLI: `sqry index` builds/saves, `sqry graph *` loads

### Important Notes
1. **No legacy code** - hooks removed in v2.0.0
2. **All plugins use GraphBuilder** via `GraphBuildHelper`
3. Architecture docs in `archive/development/ARCHIVE/FR-2025-007-unified-graph/`

---

## Coding Style

### Naming
- Modules/functions/variables: `snake_case`
- Types: `PascalCase`
- Constants: `SCREAMING_SNAKE_CASE`
- See `docs/agent-process/VARIABLE_NAMING_CONVENTIONS.md`

### Error Handling
- CLI: `anyhow::Result<T>`
- Libraries: `thiserror` custom errors
- Never panic in library code

### Memory & Concurrency
- `Arc<str>` and string interning for symbols
- `rayon` for parallelism, `parking_lot` for locks
- Avoid `tokio` unless IO-bound

### Serialization
- `bincode` for index files
- `serde_json` for CLI output

---

## Testing

- **Target**: >80% coverage, 100% for critical paths
- **Required**: `cargo test --workspace` before commit
- Use `tempfile` for isolation
- All `#[ignore]` must have reasons

---

## Reviews (MANDATORY)

Submit reviews using: `./docs/agent-process/scripts/request_review_with_uuid.sh`

```bash
./docs/agent-process/scripts/request_review_with_uuid.sh \
  --agent codex \
  --request docs/development/<feature>/[name]_review_request.md
```

See `docs/agent-process/WORKFLOW.md` for full workflow.

---

## Development Process

### Process Selection
| Change | Docs Required |
|--------|---------------|
| Bug fix ≤50 LOC | 04_PROGRESS + 06_TEST_EXECUTION |
| Feature >50 LOC | Full 6-doc pack (01-06) |
| Plugin | 3-doc pack (SPEC/IMPL/TESTS) |

### 6-Document Process
1. `01_SPEC.md` - What & Why
2. `02_DESIGN.md` - How
3. `03_IMPLEMENTATION_PLAN.md` - Steps
4. `04_PROGRESS.md` - Live status
5. `05_TEST_PLAN.md` - Verification
6. `06_TEST_EXECUTION.md` - Results

---

## Commits

**Conventional Commits** required:
- `feat`: New feature → MINOR bump
- `fix`: Bug fix → PATCH bump
- `docs`/`test`/`chore`: No bump
- `BREAKING CHANGE:` in footer → MAJOR bump

```bash
git commit -m "feat(graph): add cross-file resolution"
```

---

## Agent Rules

### File Operations
1. Verify branch: `git branch --show-current`
2. Verify folder exists: `ls <parent>/`
3. After write: Read tool to verify

### Scope Discipline
- No drive-by refactors
- No feature creep
- Fix test failures before proceeding

### File Reading Strategy
- **Large files (>25K tokens)**: Use `offset` and `limit` parameters to read in chunks
- **Codebase exploration**: Use sqry MCP tools (`mcp__sqry__*`) as the default for semantic search
- **Keyword search**: Use Grep tool for literal pattern matching
- **Open-ended questions**: Always prefer sqry semantic search over direct file reads

**Default**: When exploring the codebase or answering questions, use sqry MCP tools first:
- `mcp__sqry__semantic_search` - Find symbols by meaning
- `mcp__sqry__hierarchical_search` - RAG-optimized search with grouping
- `mcp__sqry__relation_query` - Find callers, callees, imports
- `mcp__sqry__explain_code` - Understand a symbol with context

---

## Out of Scope
- Web interface
- Metrics exporters
- Linting functionality
- Anything not serving semantic code search

---

## References
- **Review Workflow**: `docs/agent-process/WORKFLOW.md`
- **Naming Conventions**: `docs/agent-process/VARIABLE_NAMING_CONVENTIONS.md`
- **Backlog**: `docs/development/TECHNICAL_BACKLOG.md`
- **Templates**: `docs/templates/`

---

## Philosophy
sqry exists to be **the best local semantic code search tool**.
**Quality over quantity. Focus over features. Lean over bloated.**
When in doubt: **Does this make semantic search better?**
