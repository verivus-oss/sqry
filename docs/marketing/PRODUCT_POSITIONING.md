# sqry Product Positioning

**Last Updated**: 2026-01-30
**Document Version**: 1.0

---

## Category
Semantic code search and structural code intelligence (local-first).

## Positioning Statement
For engineering teams who need provable answers about their code, **sqry** is a local-first semantic search engine that builds a unified graph of code structure and relationships. Unlike platform search, sqry is workspace-wide, Git-aware, and deterministic—delivering complete, structural answers across multiple repos.

## Target Users (ICP)
- Engineering teams managing **polyrepo** services and shared libraries
- Developers who need **callers/callees/dependencies** without manual tracing
- Security, refactor, and architecture teams performing **impact analysis**
- Teams with **offline/local** workflows or strict data locality requirements

## Problem Framing
- Platform search is mostly text and repo-scoped; it doesn’t capture structure
- Cross-repo reasoning is manual and error-prone
- Probabilistic matching is insufficient for refactors, audits, or correctness work

## Core Differentiators (Code-Verified)
1. **Workspace-wide multi-repo analysis (base offering)**
   - Registry, scan, query, and stats across repositories
   - Repo-aware query filtering in one workspace
2. **Unified semantic graph**
   - Call hierarchy, dependency trees, cycles, duplicates, unused symbols
   - Cross-language edges for polyglot stacks
3. **Git-aware workflows**
   - Git-root discovery mode
   - Semantic diff across git refs
4. **Local-first + deterministic**
   - AST-backed structural matches (same query → same results)
   - Works on GitHub repos via local clones
5. **Full workflow integration**
   - CLI for automation
   - MCP for agent workflows
   - LSP for IDEs
   - VS Code extension for UI

## Competitive Frame
**Platform search** (GitHub/Bitbucket/GitLab, etc.)
- Strength: hosted text and symbol lookup
- Gap: limited structural understanding and cross-repo reasoning

**sqry**
- Strength: structure-aware, workspace-wide, Git-aware, deterministic
- Focus: developer workflows that require correctness and graph context

## Packaging & Monetization
**Base (Free)**
- Multi-repo analysis is included
- Full semantic search and graph analysis
- CLI + MCP + LSP + VS Code extension

**Paid Tier**
- **Multi-repo UI** across teams and workspaces
- **Enterprise UX** for org-scale navigation and governance workflows
- Operational polish for large deployments

## Messaging Pillars
1. **“Semantic, not just text.”**
   - Callers, dependencies, cycles, and impact in one query
2. **“Workspace-wide by default.”**
   - Multi-repo analysis is base, not gated
3. **“Local-first and deterministic.”**
   - Offline-ready, no probabilistic results
4. **“Fits real developer workflows.”**
   - CLI, IDE, and MCP integrations

## Objections & Responses
- **“We already have platform search.”**
  - Platform search is repo-scoped and text-first. sqry answers structural questions across repos.
- **“We only want a UI.”**
  - The paid tier provides multi-repo UI and enterprise UX without limiting core analysis.
- **“Is this AI search?”**
  - sqry is deterministic and AST-based, not probabilistic embedding search.

## Do-Not-Say List
- “AI search that guesses results”
- “Replaces your platform”
- “Only for single repos”

## Short Positioning Snippets
- “Semantic code search across your entire workspace.”
- “Git-aware, multi-repo by default.”
- “Deterministic answers from code structure.”
