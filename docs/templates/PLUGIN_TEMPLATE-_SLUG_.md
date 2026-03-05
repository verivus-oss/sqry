# Language Plugin Template

**Template Version**: 2.0.0  
**Created**: 2025-12-17  
**Last Updated**: 2026-03-05  
**Purpose**: Plan, implement, and validate a production-grade `sqry-lang-*` plugin with measurable semantic and polyglot tracing quality.

## Plugin Record

| Field | Value |
|-------|-------|
| Plugin crate | `sqry-lang-<language>` |
| Language | `<language-name>` |
| Tree-sitter grammar | `tree-sitter-<language>` |
| Grammar version (pinned) | `<version>` |
| Primary profile | `<systems/oop/scripting/frontend/iac/database>` |
| Secondary profile(s) | `<optional>` |
| Status | `Draft / In Progress / Complete` |
| Owner | `<name-or-agent-id>` |
| Date (UTC) | `<YYYY-MM-DD>` |

---

## Mission Fit (Required)

- Explain how this plugin improves meaning-based semantic search, not text grep.
- Explain expected contribution to polyglot trace continuity across language boundaries.
- Link this plugin work to component docs (`01_SPEC`, `02_DESIGN`, `03_IMPLEMENTATION_PLAN`, `05_TEST_PLAN`).

---

## Section 1: Feasibility Gate (Must Complete First)

Reference and complete:
- `docs/templates/LANGUAGE_FEASIBILITY_GATE.md`

Record result:

| Gate Item | Status | Evidence |
|-----------|--------|----------|
| Feasibility gate completed | [ ] | `<path-to-completed-gate-doc>` |
| Outcome (`PASS/CONDITIONAL/FAIL`) | [ ] | `<score + rationale>` |
| Approved for implementation | [ ] | `<approver + date>` |

No production implementation starts before gate outcome is `PASS` or approved `CONDITIONAL PASS`.

---

## Section 2: Capability Contract

Mark each capability as `Implemented`, `N/A`, or `Planned`.

| Capability | Required Rule | Target EdgeKind | Status | Evidence/Test |
|------------|---------------|-----------------|--------|---------------|
| Calls | Mandatory | `EdgeKind::Calls { argument_count, is_async }` | | |
| Imports | If applicable | `EdgeKind::Imports { alias, is_wildcard }` | | |
| Exports | If applicable | `EdgeKind::Exports { kind, alias }` | | |
| Inherits | If OOP language | `EdgeKind::Inherits` | | |
| Implements | If OOP language | `EdgeKind::Implements` | | |
| DbQuery | If DB language or embedded SQL | `EdgeKind::DbQuery { query_type, table }` | | |
| FfiCall | If FFI support | `EdgeKind::FfiCall { convention }` | | |
| Type refs | Strongly recommended | `EdgeKind::TypeOf { context, index, name }` | | |
| Symbol refs | Mandatory for semantic quality | `EdgeKind::References` | | |
| HTTP | If idiomatic | `EdgeKind::HttpRequest { method, url }` | | |
| gRPC | If idiomatic | `EdgeKind::GrpcCall { service, method }` | | |
| WebAssembly | If supported | `EdgeKind::WebAssemblyCall` | | |
| Message queue | If idiomatic | `EdgeKind::MessageQueue { protocol, topic }` | | |
| SQL table semantics | If SQL parser/extraction exists | `EdgeKind::TableRead` / `TableWrite` / `TriggeredBy` | | |
| Other | Language-specific | `<EdgeKind + rationale>` | | |

### Node and Metadata Contract

- Nodes: file/module/package; callable; type; variable/constant; language-specific runtime constructs.
- Metadata minimum: name, qualified name, location/span, visibility/modifiers, signature, return type, async/static where applicable.

---

## Section 3: Implementation Design

### 3.1 Plugin Surface

- `metadata()` values and language ID conventions.
- `extensions()` list and precedence rules.
- `language()` and parser configuration rules.
- `parse_ast()` behavior for malformed input (no panics).
- `extract_scopes()` coverage and scope nesting behavior.
- Legacy `graph_builder()` usage vs unified graph path plan (if applicable).

### 3.2 Graph Build Mapping

Define grammar capture -> graph mapping:

| Grammar Construct | Node/Edge Emitted | Confidence Rule | Notes |
|-------------------|-------------------|-----------------|-------|
| `<construct>` | `<node/edge>` | `<exact/heuristic>` | |
| `<construct>` | `<node/edge>` | `<exact/heuristic>` | |

### 3.3 Ambiguity and Dynamic Behavior

- Resolution strategy for reflection/eval/metaprogramming/dynamic dispatch.
- Rules for unresolved targets (emit partial edge + confidence note; never silent drop).
- Security bounds for parsing untrusted source.

---

## Section 4: Fixtures and Tests

### 4.1 Required Fixture Set

Store fixtures under `sqry-lang-<language>/tests/fixtures/`.

| Fixture | Purpose | Required |
|--------|---------|----------|
| `simple.<ext>` | basic syntax and calls/imports | Yes |
| `complex.<ext>` | nested/class/module composition | Yes |
| `edge_cases.<ext>` | boundary semantics (wildcards, variadics, anonymous, etc.) | Yes |
| `error_handling.<ext>` | malformed but parseable inputs | Yes |
| `real_world.<ext>` | production-like sample | Yes |
| `cross_boundary.<ext>` | FFI/HTTP/gRPC/DB/WASM/MQ scenario (if applicable) | Conditional |

### 4.2 Test Matrix

| Test Type | Minimum | Target | Notes |
|----------|---------|--------|-------|
| Unit tests | 10 | 15+ | edge extraction and metadata |
| Integration tests | 3 | 5+ | fixture-driven |
| Negative tests | 3 | 5+ | malformed input/auth/path issues |
| Cross-language trace tests | 1 | 3+ | where applicable |

### 4.3 Mandatory Commands

```bash
cargo fmt --all
cargo test -p sqry-lang-<language>
cargo clippy -p sqry-lang-<language> -- -D warnings
```

If workspace-impacting code is touched, run workspace-level checks per repo policy.

---

## Section 5: Polyglot Trace Validation (Required)

Define and validate at least one end-to-end trace path when the language supports boundaries.

| Scenario | Source | Boundary Type | Target | Expected Edge Chain | Status |
|----------|--------|---------------|--------|---------------------|--------|
| `<name>` | `<symbol>` | `FFI/HTTP/gRPC/DB/WASM/MQ` | `<symbol/service/table>` | `<edge-sequence>` | |

Required outcomes:
- [ ] At least one valid cross-boundary trace for applicable languages.
- [ ] Trace preserves location metadata for navigation.
- [ ] Uncertain links are explicit (confidence/ambiguity behavior documented).

---

## Section 6: Performance, Quality, and Release Readiness

### 6.1 Performance Targets

| Metric | Target | Measured | Evidence |
|--------|--------|----------|----------|
| Parse + extract latency (1000-line file) | `<100ms target context-aware>` | | |
| Memory behavior | No unbounded growth | | |
| Determinism | Same input -> same graph output | | |

### 6.2 Quality Gates

- [ ] No panics in library paths for malformed input.
- [ ] Deterministic graph output on golden fixtures.
- [ ] Docs updated (`README`, development docs, troubleshooting if needed).
- [ ] Known limitations documented explicitly.
- [ ] Required multi-agent reviews completed and resolved (Codex/Gemini/Claude or approved equivalents).

### 6.3 Release Readiness Decision

| Decision | Criteria |
|----------|----------|
| PASS | All required checks complete; no blocking gaps |
| CONDITIONAL PASS | Non-blocking gaps with dated remediation |
| FAIL | Missing mandatory capability/test/quality requirements |

**Decision**: `<PASS / CONDITIONAL PASS / FAIL>`  
**Rationale**: `<short rationale>`

---

## Section 7: Sign-off

- Implementer: `<name-or-agent-id>` Date: `<YYYY-MM-DD>`
- Reviewer: `<name-or-agent-id>` Date: `<YYYY-MM-DD>`
- Ready for workspace integration: [ ]

---

## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.
