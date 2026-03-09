# Language Feasibility Gate Checklist

**Template Version**: 2.0.0  
**Created**: 2025-12-17  
**Last Updated**: 2026-03-05  
**Purpose**: Score language plugins against a single, measurable quality bar for semantic search and cross-language tracing

---

## Mission Fit (Non-Negotiable)

sqry is a **lean, focused semantic code search tool** in Rust (**v1.90+**, Edition 2024).  
A language plugin is feasible only if it improves search by **meaning** (semantic structure and relations), not text matching.

---

## Universal Plugin Measurement List (Applies to Current + Future Languages)

Use this as the shared benchmark for every plugin in `sqry-lang-*`.

| ID | Measurement | Required Threshold | Evidence |
|----|-------------|--------------------|----------|
| M1 | Grammar maturity | Active grammar + pinned version + maintainer activity checked | Grammar repo URL, version, recent commits/issues |
| M2 | Parse fidelity | >=99% valid fixture parse success; no crashes/panics | Fixture parse logs + test output |
| M3 | Core node extraction | file/module/package; callable; type; variable/constant nodes extracted | Node inventory with fixture references |
| M4 | Core structural edges | `Defines`, `Contains`, `References`, `TypeOf` emitted where language allows | Edge assertions in tests |
| M5 | Call semantics | `Calls { argument_count, is_async }` coverage >=95% on golden fixtures | Edge accuracy report |
| M6 | Module semantics | `Imports`/`Exports` with alias/wildcard/kind fidelity >=95% when language supports them | Import/export fixture tests |
| M7 | OOP semantics | `Inherits`/`Implements` correctness >=95% for OOP languages | Class/interface fixture tests |
| M8 | Data semantics | `DbQuery { query_type, table }` OR `TableRead`/`TableWrite`/`TriggeredBy` for DB languages or embedded SQL | SQL/DB fixture tests |
| M9 | Interop semantics | `FfiCall { convention }` where language supports native/foreign calls | FFI fixture tests |
| M10 | Service semantics | `HttpRequest`, `GrpcCall`, `WebAssemblyCall`, `MessageQueue` emitted when idiomatic in language/ecosystem | Cross-service fixture tests |
| M11 | Metadata completeness | name, qualified_name, location, visibility/modifiers, signature/return, async/static captured >=95% | Metadata completeness report |
| M12 | Polyglot traceability | At least one validated path from this plugin into another domain/language when applicable | End-to-end trace tests |
| M13 | Performance budget | Meets repo targets (<100ms first result p90; indexing targets unchanged) | Benchmark results |
| M14 | Stability + maintainability | Deterministic output, no flaky tests, docs + fixtures complete | CI history + docs checklist |

---

## Capability Profile Selection

Choose one primary profile and any secondary profiles. This determines mandatory vs optional edges.

| Profile | Typical Plugins | Mandatory Targets | Optional Stretch Targets |
|---------|------------------|-------------------|--------------------------|
| Systems + native interop | `c`, `cpp`, `rust`, `go`, `zig`, `swift` | Calls, Imports, Exports (if language/module supports), TypeOf, References, FfiCall | HttpRequest, GrpcCall, WebAssemblyCall |
| OOP/runtime platform | `java`, `kotlin`, `scala`, `groovy`, `csharp`, `salesforce-apex`, `sap-abap`, `servicenow-xanadu` | Calls, Imports, Exports (where supported), Inherits, Implements, TypeOf, References | FfiCall, HttpRequest, DbQuery, MessageQueue |
| Dynamic scripting | `javascript`, `typescript`, `python`, `ruby`, `php`, `perl`, `lua`, `shell`, `r` | Calls, Imports, Exports (where supported), References, TypeOf | Inherits, Implements, FfiCall, DbQuery, HttpRequest |
| Frontend/component markup | `html`, `css`, `svelte`, `vue` | Contains, Defines, Imports, Exports (where supported), Calls (event handlers/component invocations), References | HttpRequest, MessageQueue, WebAssemblyCall |
| IaC/config graph | `terraform`, `pulumi`, `puppet` | Defines, Contains, References, Imports (module/provider), Exports (outputs where supported) | HttpRequest, MessageQueue, DbQuery |
| Database/procedural SQL | `sql`, `oracle-plsql` | DbQuery, TableRead, TableWrite, TriggeredBy, Calls (procedures/functions), Imports/exports equivalent if available | HttpRequest, MessageQueue |

---

## Research-Based Capability Expectations (Official-Spec Driven)

Validate assumptions against official references before implementation. Minimum evidence set:

1. Module/import/export model (language spec/docs)
2. Type/object model (inheritance/interfaces/traits/protocols where applicable)
3. Interop model (`FFI`, `extern`, `JNI`, `cgo`, `P/Invoke`, `dart:ffi`, etc.)
4. Data/query model (native SQL/procedural SQL constructs)
5. Service-call model (HTTP/gRPC/message buses if idiomatic)

Reference families used for baseline expectations include:
- Rust Reference + Rustonomicon FFI
- Go Spec + `cmd/cgo`
- C/C++ reference docs (`#include`, linkage, inheritance)
- Java JLS + JNI
- C# language reference + `using`/namespace docs
- Kotlin docs (packages/imports/inheritance/interfaces/native interop)
- Dart docs (`import/export`, mixins, `dart:ffi`)
- Swift docs (inheritance/protocols/C & Objective-C interop)
- JavaScript/TypeScript module and type-system docs
- Python import/class docs + `ctypes`/`cffi`
- PHP namespaces/OOP/FFI docs
- Elixir module/import/protocol/behaviour docs + Rustler/NIF ecosystem
- Haskell report modules + FFI chapter
- PostgreSQL SQL command docs + Oracle PL/SQL package/trigger docs
- Terraform/Pulumi/Puppet language docs
- HTML/CSS/Vue/Svelte official docs/specs

---

## Target Capability Checklist (Per Plugin)

Mark every line as `Implemented`, `N/A`, or `Planned`.

| Capability | Required Rule | Target EdgeKind | Status | Notes |
|------------|---------------|-----------------|--------|-------|
| Calls | Mandatory | `EdgeKind::Calls { argument_count, is_async }` | | |
| Imports | If applicable | `EdgeKind::Imports { alias, is_wildcard }` | | |
| Exports | If applicable | `EdgeKind::Exports { kind, alias }` | | |
| Inherits | If OOP language | `EdgeKind::Inherits` | | |
| Implements | If OOP language | `EdgeKind::Implements` | | |
| DbQuery | If DB language or embedded SQL | `EdgeKind::DbQuery { query_type, table }` | | |
| FfiCall | If FFI support | `EdgeKind::FfiCall { convention }` | | |
| Type refs | Strongly recommended | `EdgeKind::TypeOf { context, index, name }` | | |
| Symbol refs | Mandatory for semantic search quality | `EdgeKind::References` | | |
| HTTP | If language commonly issues API calls | `EdgeKind::HttpRequest { method, url }` | | |
| gRPC | If ecosystem commonly uses gRPC | `EdgeKind::GrpcCall { service, method }` | | |
| WebAssembly | If wasm host/guest support exists | `EdgeKind::WebAssemblyCall` | | |
| MQ | If publish/subscribe is idiomatic | `EdgeKind::MessageQueue { protocol, topic }` | | |
| SQL table semantics | If SQL parser or SQL extraction exists | `EdgeKind::TableRead` / `TableWrite` / `TriggeredBy` | | |
| Other | Language-specific | `<EdgeKind variant + rationale>` | | |

---

## Polyglot Cross-Language Tracing Gate

A plugin is not "best-in-class" unless it contributes to cross-language trace continuity.

### Required Trace Contracts

- [ ] **Interop contract**: map language-native interop syntax to `FfiCall` with normalized convention metadata.
- [ ] **Service contract**: map HTTP/gRPC/message patterns to normalized service edges.
- [ ] **Data contract**: capture DB operations with query type + table/collection when determinable.
- [ ] **Boundary contract**: preserve source/target spans so traces can cross files/crates/languages.
- [ ] **Ambiguity contract**: unknown targets are emitted with explicit uncertainty notes, never silently dropped.

### Minimum End-to-End Scenarios (Per Applicable Plugin)

- [ ] In-language call chain (`file A -> file B`).
- [ ] Cross-module/package chain (`module -> imported symbol`).
- [ ] Cross-boundary chain (one of: `FFI`, `HTTP`, `gRPC`, `DB`, `WASM`, `MQ`).
- [ ] At least one trace that joins with another plugin's nodes/edges.

---

## Scoring and Decision

### Weighted Score (100 points)

| Area | Weight | Pass Condition |
|------|--------|----------------|
| AST + parsing reliability (M1-M3) | 20 | >=16/20 |
| Semantic edge quality (M4-M10) | 40 | >=32/40 |
| Metadata quality (M11) | 10 | >=8/10 |
| Polyglot trace quality (M12) | 20 | >=16/20 |
| Performance + stability (M13-M14) | 10 | >=8/10 |

### Hard Fails (Automatic FAIL)

- Parse fidelity below 99% on valid fixtures.
- Missing `Calls` support.
- Missing `References` support.
- Claims cross-language support but emits no normalized cross-boundary edges.
- No reproducible test evidence.

### Outcomes

- **PASS**: Score >=85 and no hard fails.
- **CONDITIONAL PASS**: Score 75-84, no hard fails, explicit remediation plan with deadline.
- **FAIL**: Score <75 or any hard fail.

---

## Plugin Assessment Record

| Field | Value |
|-------|-------|
| Language | `<language-name>` |
| Plugin Crate | `sqry-lang-<language>` |
| Primary Profile | `<profile>` |
| Secondary Profiles | `<profile-optional>` |
| Tree-sitter Grammar | `tree-sitter-<language>` |
| Grammar Version | `<version>` |
| Assessor | `<agent-id>` |
| Assessment Date (UTC) | `<YYYY-MM-DD>` |
| Score | `<0-100>` |
| Decision | `PASS / CONDITIONAL PASS / FAIL` |

**Blockers / Gaps**

1. `<blocker + evidence + mitigation>`
2. `<blocker + evidence + mitigation>`

**Sign-off**

- Assessor: `<agent-id>` Date: `<YYYY-MM-DD>`
- Reviewer: `<agent-id>` Date: `<YYYY-MM-DD>`

---

## Implementation Expectations for New Languages

Before writing production plugin code:

1. Complete this gate with evidence for all applicable measurements.
2. Build fixture pack that covers happy path, edge cases, malformed input, and real-world code.
3. Define mapping from grammar nodes to sqry nodes/edges with ambiguity handling rules.
4. Define cross-language boundary extraction strategy (FFI/HTTP/gRPC/DB/WASM/MQ).
5. Complete required multi-agent reviews (Codex/Gemini/Claude or approved equivalents) and resolve findings.
6. Commit test plan + expected trace outputs.

No implementation starts until gate outcome is `PASS` or approved `CONDITIONAL PASS`.

---

## Document History

| Date | Change |
|------|--------|
| 2025-12-17 | Initial template creation as Wave 0 deliverable 0.8 |
| 2026-03-05 | Replaced with measurable universal plugin gate: capability scoring, profile-based targets, and polyglot cross-language tracing requirements |
