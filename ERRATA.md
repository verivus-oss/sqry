# ERRATA: Architectural Pivot - FR-2026-001 & Natural Language Interface

**Date**: 2025-11-21
**Impact**: Critical - affects FR-2026-001 specification, implementation, and user-facing documentation
**Status**: Active - Remediation in progress (Stabilization Sprint 2025-Q4, Stream 5)

---

## Summary

A fundamental architectural misunderstanding was identified in FR-2026-001 and propagated across 89 files in the codebase. The specification incorrectly described LLMs/embeddings **searching the codebase**, when they should only **translate natural language to SQRY commands**.

**This errata supersedes** all prior documentation describing "hybrid search", "embedding search", or "LLM-based retrieval" as part of SQRY's core search functionality.

Going forward, **FR-2026-001 is defined exclusively by the NL translation respec**:
- Canonical spec: `
- Design: `
- Guide: `docs/NL_TRANSLATION_GUIDE.md`

The original hybrid-embeddings spec is **archived and non-authoritative** for any new implementation work.

---

## The Misunderstanding

### ❌ What Was Incorrectly Specified

**FR-2026-001: "Natural Language Search via Hybrid Embeddings"** described:

```
User Query: "find authentication logic"
    ↓
Stage 1: Coarse Retrieval (embeddings search 100K code chunks)  ← SLOW!
    ↓
Stage 2: Fine Reranking (embeddings rerank top-100 candidates)  ← SLOW!
    ↓
Stage 3: AST/Graph Fusion
    ↓
Results
```

**Problem**: This architecture puts LLMs/embeddings **in the search path**, making queries take **seconds** instead of **nanoseconds**.

**Invalid Assumption**: *"LLMs/embeddings would be faster than ripgrep to find and analyze information"*

**Reality**:
- **ripgrep/AST/graph = nanoseconds** (SQRY's core competency)
- **LLMs/embeddings = seconds** (orders of magnitude slower)

---

## The Correct Architecture

### ✅ What FR-2026-001 SHOULD Specify

**Natural Language Interface via Command Translation**:

```
User/AI Agent: "find authentication logic"
    ↓
Mini LLM: Translates NL → SQRY command syntax
    ↓ output: sqry query "function:*auth* OR class:*Auth*"
    ↓
SQRY Core: Executes command (NANOSECONDS - ripgrep/AST/graph)
    ↓
Results/Diagrams
```

**Key Principles**:
1. **LLMs translate, they do NOT search**
2. **SQRY searches** (nanoseconds - ripgrep/AST/graph speed)
3. **Never** put LLMs in the critical search path

---

## Use Cases for Mini LLMs (Correctly Scoped)

### Use Case 1: User Convenience Layer

**Traditional SQRY** (requires learning syntax):
```bash
sqry query "function:authenticate AND NOT test"
sqry query "class:*Controller callers:*"
```

**With NL Translation** (convenient):
```bash
sqry ask "find authentication functions, exclude tests"
sqry ask "which controllers have callers"
```

**Flow**: LLM translates → SQRY executes (nanoseconds) → User sees results instantly

---

### Use Case 2: Teaching AI Agents How to Use SQRY (Critical!)

**The Real Value**: Mini LLMs teach AI agents/MCP clients how to use SQRY's command syntax.

#### Example: Codex/Claude via MCP

**Without NL Layer** (Codex/Claude must learn SQRY syntax):
```
User: "Find authentication logic in this codebase"
Codex/Claude: *tries to use grep* (wrong tool, misses semantic context)
```

**With NL Layer** (LLM teaches Codex/Claude):
```
User: "Find authentication logic in this codebase"
    ↓
Codex/Claude → MCP → SQRY ask "authentication logic"
    ↓
Mini LLM: Translates to: sqry query "function:*auth* OR class:*Auth*"
    ↓
SQRY: Executes (nanoseconds) → Results to Codex/Claude
    ↓
Codex/Claude: Provides context-aware answer to user
```

**Benefit**: AI agents don't need to learn SQRY syntax - the mini LLM bridges the gap.

#### Example: Cursor/Windsurf Integration

**Scenario**: Developer asks Cursor, "Where is error handling for API calls?"

```
Cursor/Windsurf IDE
    ↓
MCP Request: "error handling for API calls"
    ↓
SQRY NL Layer: sqry query "function:*error* AND function:*api*"
    ↓
SQRY Core: Nanosecond search → Results
    ↓
Cursor: Shows inline results in editor
```

**Without this layer**: Each AI agent would need custom SQRY query logic.

**With this layer**: All agents speak natural language → mini LLM translates → SQRY searches fast.

---

## Architectural Comparison

### Traditional Search Tools (grep/ripgrep)

```
User: Writes regex pattern
    ↓
Tool: Text search (fast)
    ↓
Results: Text matches (no semantic understanding)
```

**Limitation**: No code structure awareness (finds text, not symbols/functions/classes).

---

### SQRY Core (Phase 1 & 2)

```
User: Writes SQRY query (requires learning syntax)
    ↓
SQRY: AST/graph search (NANOSECONDS)
    ↓
Results: Semantic matches (functions, classes, callers, etc.)
```

**Advantage**: Understands code structure.
**Limitation**: Requires learning SQRY query syntax.

---

### SQRY with NL Layer (Phase 3 - Correct Architecture)

```
User/AI Agent: Natural language question
    ↓
Mini LLM: Translates NL → SQRY command (1-2 seconds)
    ↓
SQRY: AST/graph search (NANOSECONDS)
    ↓
Results: Semantic matches + fast delivery
```

**Advantages**:
- Maintains nanosecond search speed (LLM only translates, doesn't search)
- Convenient for humans (no syntax learning required)
- **Critical**: Teaches AI agents how to use SQRY via MCP
- Preserves local-first, air-gapped operation (mini LLMs bundled/cached)

---

### ❌ FR-2026-001 Original Spec (Incorrect)

```
User: Natural language question
    ↓
Embeddings: Encode query + search 100K code chunks (SECONDS)
    ↓
Reranking: LLM reranks top-100 (MORE SECONDS)
    ↓
AST Fusion: Validate with graph (finally uses SQRY's core)
    ↓
Results: Semantically relevant but SLOW
```

**Problems**:
- LLMs in search path → seconds instead of nanoseconds
- Violates SQRY's "lean, focused, fast" mission
- Embeddings search is slower than ripgrep (defeats the purpose!)

---

## Impact Assessment

### Files Affected: 89 Total

**Breakdown**:
- **Tier 1**: 16 files (FR-2026-001 specs, critical)
- **Tier 2**: 12 files (core implementation - `hybrid.rs`, `embeddings/`, tests)
- **Tier 3**: 6 files (CLI args, integration tests)
- **Tier 4**: 15 files (user documentation - README, guides)
- **Tier 5**: 6 files (RKG nodes, requirements)
- **Tier 6**: 9 files (audit documents - mark as errata)
- **Tier 7**: 4 files (experiments - archive or repurpose)
- **Tier 8**: 21 files (misc project files)

**Full Inventory**: (internal docs removed)

---

## Remediation Plan

### Phase 1: Freeze FR-2026-001 (Immediate)

- [x] **STOP** all FR-2026-001 implementation work under the hybrid-embeddings architecture
- [x] Mark FR-2026-001 (hybrid) status: "ON HOLD - Architecture Pivot Required" and archive original spec/docs
- [x] Create RFC and respec: "NL Translation (not Embedding Search)" – now the only valid FR-2026-001 design

---

### Phase 2: Stream 5 - Messaging Correction (This Week)

**Part of Stabilization Sprint 2025-Q4**

**Scope** (2-3 days):
1. [ ] Update [README.md](README.md)
   - Remove "hybrid search" claims
   - Clarify: "Nanosecond search with optional NL convenience layer"
   - Emphasize: "LLMs translate, SQRY searches"

2. [ ] Rename & Rewrite Documentation
   - `docs/HYBRID_SEARCH_GUIDE.md` → `docs/NL_TRANSLATION_GUIDE.md`
   - Focus on: LLM translation use cases (user convenience + AI agent teaching)
   - Clarify: SQRY core remains nanosecond fast

3. [ ] Update RKG
   - Mark FR-2026-001 node for respec
   - Add errata reference to node metadata
   - Document architectural pivot in RKG history

4. [x] Create This ERRATA.md
   - Document the misunderstanding
   - Explain correct architecture
   - Reference from all affected documents

**Exit Criteria**:
- [ ] No user-facing doc claims LLMs/embeddings search code
- [ ] Clear messaging: "LLMs translate, SQRY searches (nanoseconds)"
- [ ] AI agent/MCP use case documented
- [ ] FR-2026-001 hybrid-embeddings spec clearly archived; NL translation respec referenced as the only active design

---

### Phase 3: Code Remediation (Post-Stabilization, 1-2 weeks)

**Scope**:
1. [x] Remove `sqry-core/src/query/hybrid.rs` and related hybrid executor wiring from `QueryExecutor`.
2. [x] Remove `sqry-core/src/embeddings/` and `sqry-core/src/vector/` (hybrid embeddings/vector storage).
3. [x] Remove hybrid-specific CLI paths/features (`embeddings`, `embeddings-full`, manifest tooling) from `sqry-cli`.
4. [x] Remove `sqry-core/tests/hybrid_e2e_golden.rs` and associated `tests/fixtures/hybrid` golden fixtures.
5. [ ] Implement NL translation interface (`sqry ask` / `sqry_ask`) per FR-2026-001 (Respec).

**Exit Criteria**:
- [x] No code implements LLM/embedding-based code search (hybrid path removed).
- [ ] NL translation interface functional.
- [ ] MCP integration teaches AI agents SQRY syntax.
- [x] All existing tests/builds pass without embeddings features.

---

### Phase 4: Full Documentation Sweep (Post-Stabilization, 2-3 weeks)

**Scope**: Systematic update of all 89 files

**Search & Replace Strategy**:
```bash
# Find problematic terms
grep -r "hybrid search" docs/
grep -r "embedding search" docs/
grep -r "LLM.*retrieval" docs/
grep -r "semantic search" docs/  # Context-dependent!

# Replace with correct terms
"hybrid search"     → "NL translation" or "nanosecond search"
"embedding search"  → "command generation" or "query translation"
"LLM retrieval"     → "LLM-based command translation"
"semantic search"   → "AST/graph-based search (nanoseconds)"
```

**Key Messages to Reinforce**:
1. **SQRY Core = Nanoseconds**: ripgrep/AST/graph search speed
2. **LLMs = Convenience**: Translate NL → commands (never search)
3. **AI Agent Teaching**: Mini LLMs bridge gap between agents and SQRY syntax
4. **Local-First Maintained**: Mini LLMs bundled/cached, no cloud dependencies

---

## Key Stakeholders

### Internal (Development Team)
- **Action**: Review this errata before any FR-2026-001 work
- **Validation**: Ensure all new code aligns with corrected architecture
- **Testing**: Validate nanosecond search speed is preserved

### External (Users)
- **Communication**: README updated to clarify NL translation vs. search
- **Documentation**: Guides emphasize SQRY's speed advantage
- **MCP Users**: AI agents (Claude, Codex, Gemini, Cursor, Windsurf) benefit from NL bridge

### AI Agent Developers (MCP Integrators)
- **Benefit**: Don't need to learn SQRY syntax - mini LLM translates
- **Integration**: MCP JSON-RPC interface remains unchanged
- **Example Workflow**:
  ```json
  // MCP Request
  {
    "tool": "sqry_ask",
    "params": {
      "query": "find authentication logic"
    }
  }

  // SQRY translates (mini LLM) → executes (nanoseconds) → returns
  {
    "results": [
      {"symbol": "authenticate_user", "file": "auth.rs", "line": 42},
      {"symbol": "Auth::login", "file": "auth.rs", "line": 103}
    ]
  }
  ```

---

## Lessons Learned

### What Went Wrong

1. **Specification Error**: FR-2026-001 was written with embedding-based search architecture without validating against SQRY's core mission (nanosecond speed).

2. **Invalid Assumption**: Assumed LLMs/embeddings would be faster than ripgrep for code search (opposite of reality).

3. **Scope Creep**: Phase 3 (NL convenience layer) became "hybrid search" (fundamental architecture change) instead of translation layer (interface enhancement).

4. **Missed Use Case**: Focused on human users, missed the critical AI agent teaching use case (MCP integration).

---

### What Went Right

1. **Early Detection**: Caught before widespread implementation (only experimental code exists).

2. **Multi-Agent Review**: User correction during stabilization planning prevented shipping incorrect architecture.

3. **Evidence-Based**: 89 files inventoried with tiered remediation plan.

4. **Clear Path Forward**: NL translation architecture is well-defined, preserves core speed advantage.

---

## Success Criteria for Correction

### Technical Validation
- [ ] SQRY core search remains nanosecond-fast (no LLMs in search path)
- [ ] NL translation adds <2 seconds overhead (acceptable for convenience)
- [ ] MCP integration teaches AI agents SQRY syntax automatically

### Messaging Validation
- [ ] User-facing docs clearly state: "LLMs translate, SQRY searches"
- [ ] No claims of "LLM-based search" or "embedding search"
- [ ] AI agent use case prominently featured (MCP integration value prop)

### Process Validation
- [ ] FR-2026-001 respec approved before any implementation
- [ ] All 89 files updated to reflect correct architecture
- [ ] Future specs validated against core mission (nanosecond search)

---

## References

### Remediation Documents
- [Architectural Misalignment Inventory]() - Complete list of 89 affected files
- [Stabilization Sprint 2025-Q4]() - Includes Stream 5 (Messaging Correction)
- [Stream 5 Implementation Plan]() - Detailed remediation tasks

### Original Documents (Superseded)
- ⚠️ **FR-2026-001** (ON HOLD - Architecture Pivot Required)
  - [01_SPEC.md]() - Incorrect embedding search spec
  - [02_DESIGN.md]() - Incorrect embedding search design
  - All FR-2026-001 docs marked for respec

### Core Mission Documents (Still Valid)
- [AGENTS.md](AGENTS.md) - Ground rules (no stubs, quality first, validate before change)
- [README.md](README.md) - Will be updated to remove "hybrid search" claims

---

## Version History

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2025-11-21 | Initial errata creation - documented architectural pivot |

---

## Questions or Concerns?

If you're working on SQRY and have questions about the correct architecture:

1. **Read This Errata**: Understand the misunderstanding and correct approach
2. **Review Inventory**: Check if your work involves any of the 89 affected files
3. **Validate Against Core Mission**: Does your change preserve nanosecond search speed?
4. **Ask Before Implementing**: If unsure, reference this errata in design discussions

**Contact**: Development team via AGENTS.md multi-agent review process

---

**Status**: Active - Remediation in progress (Stream 5 executing)
**Next Review**: Post-stabilization sprint (after Stream 5 completion)
