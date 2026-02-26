# sqry Marketing One-Pager

**Last Updated**: 2026-01-30
**Document Version**: 1.0

---

## One-Line Summary
sqry is a local-first, semantic code search engine that understands code structure and relationships, letting developers query across an entire workspace (including multiple repos) with deterministic results.

## The Problem
- Text search and hosted platform search miss structure (callers, dependencies, impacts)
- Repo-scoped tools make cross-repo reasoning slow and error-prone
- Developers need fast, trustworthy answers directly from code—not best-guess matches

## The Solution
sqry builds a unified, AST-backed graph of your code and answers questions by meaning. It’s Git-aware, cross-repo by default, and integrates into CLI, IDEs, and MCP workflows.

## What You Get (Base)
- **Workspace-level multi-repo analysis** with repo-aware queries and aggregate stats
- **Semantic search** (structure-aware, deterministic results)
- **Graph analysis**: call hierarchy, dependency trees, cycles, duplicates, unused symbols, impact analysis
- **Cross-language edges** for polyglot codebases
- **Git-aware workflows**: git-root discovery + semantic diffs across refs
- **Local-first indexing** with unified graph snapshots
- **Interfaces**: CLI, MCP server, LSP server, VS Code extension

## Paid Tier (Not Core Capability Gating)
- **Multi-repo UI** across workspaces and teams
- **Enterprise UX** (org-scale navigation, governance-oriented workflows, curated views)
- **Operational polish** for large orgs and compliance-minded teams

## Differentiators vs Platform Search
- **Semantic, not just text**: ask “who calls this?” and “what breaks if I change it?”
- **Workspace-wide**: query across repos by default, not one index at a time
- **Local-first**: works offline, works on GitHub repos via local clones
- **Deterministic**: same query → same results (no probabilistic embedding matches)

## Ideal Users & Use Cases
- **Polyrepo engineering teams** (microservices + shared libraries)
- **Security, refactor, and dependency impact audits**
- **Cross-language migration and cleanup projects**
- **Developer productivity workflows in CLI/IDE/MCP agents**

## Call to Action
Index your workspace and run a semantic query in minutes. sqry turns codebases into navigable graphs you can actually reason about.
