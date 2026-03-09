# MCP Integration Planning Template

Use this template when adding or modifying sqry MCP tools, resources, prompts,
server configuration, transport behavior, or security controls. Populate it
during planning and store it with the component pack (for example,
`docs/development/<component>/MCP_INTEGRATION-_SLUG_.md`).

## 1. Integration & UX Summary
- **MCP surface/entry point:** `sqry-mcp` (or related integration point).
- **Primary user workflow:** Brief user story enabled by this MCP change.
- **Semantic search alignment:** How this improves meaning-based search/tracing.
- **Clients in scope:** Codex, Claude, Gemini, VSCode, custom MCP clients.
- **Docs to update/create:** Setup docs, troubleshooting, examples, release notes.


## 2. Tools/Resources/Prompts Contract
| Surface | Name | Input Schema | Output Schema | Streaming | Deterministic | Notes |
|---------|------|--------------|---------------|-----------|---------------|-------|
| Tool | `example_tool` | `<json-schema-ref>` | `<json-schema-ref>` | No | Yes | Pagination/limits |
| Resource | `example://uri` | `N/A` | `<shape>` | N/A | Yes | Access scope |
| Prompt | `example_prompt` | `<shape>` | `<shape>` | N/A | Partial | Prompt vars |

- Enumerate request/response compatibility requirements.
- Specify backward compatibility expectations for existing clients.
- Record pagination/filter conventions (`max_results`, `page_token`, etc.).


## 3. Execution Model & Runtime Constraints
- **Workspace root behavior:** Path restrictions and validation strategy.
- **Index dependencies:** Required index state and auto-indexing behavior.
- **Feature flags/env vars:** `SQRY_MCP_*` flags, defaults, fail-open/fail-closed.
- **Concurrency model:** Threading, queueing, and cancellation semantics.
- **Failure mode policy:** Timeout/retry behavior and graceful degradation.


## 4. Transport, AuthN, and AuthZ
- **Transport mode:** stdio / HTTP / SSE / other.
- **Authentication model:** Local trust, OIDC, token, mTLS, or N/A.
- **Authorization model:** Tool-level allow/deny policies and scope checks.
- **Secret handling:** Source of secrets, redaction, rotation, audit policy.
- **Identity propagation:** Request identity/tenant/workspace mapping rules.


## 5. Safety, Policy, and Data Handling
- Tool risk classification (read-only, mutating, external side effects).
- Allowed path and command boundaries.
- PII/secret redaction behavior in outputs and logs.
- Prompt-injection hardening expectations for tool/resource inputs.
- Guardrails for destructive operations and escalation paths.


## 6. Cross-Language Tracing Contract (Polyglot Requirement)
- Define how MCP outputs preserve sqry graph semantics across languages.
- Map each relevant tool to edge families (`Calls`, `Imports`, `Exports`,
  `Inherits`, `Implements`, `DbQuery`, `FfiCall`, `HttpRequest`, `GrpcCall`,
  `WebAssemblyCall`, `MessageQueue`, `References`, `TypeOf`).
- Require at least one end-to-end trace path crossing a language boundary
  where the use case allows.
- Document ambiguity behavior (unknown target, partial resolution, confidence).


## 7. Output Semantics and Error Taxonomy
- **Human-readable output:** Stability and ordering guarantees.
- **Machine output:** Schema versioning and compatibility strategy.
- **Error classes:** Validation, auth, index state, execution, timeout, internal.
- **Error payload shape:** code/message/details/retryable fields.
- **Client behavior contract:** Which errors are retryable vs terminal.

## 8. Performance and Reliability Targets
| Metric | Target | Measurement Method | Notes |
|--------|--------|--------------------|-------|
| Tool latency (p50/p95) | `<target>` | Bench + integration tests | By tool class |
| Throughput | `<target>` | Load tests | Concurrent clients |
| Timeout rate | `<target>` | CI + soak tests | Under realistic workload |
| Memory ceiling | `<target>` | Profiling | Include index-heavy flows |
| Determinism rate | `100%` on golden tests | Snapshot comparison | Same input => same output |

## 9. Test Strategy (Pre-Implementation)
- Contract tests for all tool/resource schemas.
- Integration/e2e tests against real MCP clients where feasible.
- Negative tests: malformed payloads, unauthorized access, missing index.
- Resilience tests: cancellation, timeouts, transport disconnect/reconnect.
- Cross-language trace tests validating polyglot path continuity.
- Required multi-agent reviews completed and resolved (Codex/Gemini/Claude or approved equivalents).

## 10. Documentation & Rollout Plan
- Docs and examples to publish before implementation.
- CLI/setup updates (`sqry mcp ...`, env vars, configuration snippets).
- Troubleshooting additions and known-limitations entries.
- Release notes and migration guidance for client behavior changes.

## 11. Risks & Open Questions
- Outstanding architecture decisions.
- Dependency risks (protocol, crates, external identity systems).
- Security/compliance concerns and mitigation owners.
- Rollback plan (feature flag or safe fallback) if release degrades clients.

---
## Token Optimization (Required)
Use `docs/TOKEN_OPTIMIZATION_GUIDE.md`.
- Dense phrasing; drop filler/articles when safe.
- Prefer lists/tables; avoid narrative blocks.
- One sentence per bullet; avoid hedging.
- Use snake_case; standard names.
- Compact `{id,name}`; inline `field:type!>0`.


> **Reminder:** Attach the completed template to the planning pack and reference
> it from Spec, Design, Implementation Plan, and Test Plan docs. Keep it aligned
> with user-facing MCP documentation as decisions evolve.
