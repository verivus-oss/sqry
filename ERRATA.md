# ERRATA: Natural Language Interface Architecture

**Date**: 2025-11-21
**Affects**: Documentation and architecture for natural language queries (`sqry ask`, `sqry_ask` MCP tool)

---

## Summary

Early documentation for sqry's natural language feature incorrectly described an architecture where LLMs and embeddings would **search** the codebase. The correct architecture - and the one actually implemented - uses LLMs only to **translate** natural language into sqry query syntax. sqry's core search engine (AST/graph) handles the actual search.

This errata supersedes any remaining references to "embedding search" or "LLM-based retrieval" as part of sqry's search functionality. (Note: "hybrid search" referring to combined AST + text fallback is unrelated and still valid.)

---

## What Changed

### The incorrect description

An earlier design described a pipeline where embeddings would search code chunks and an LLM would rerank results before passing them to sqry's graph engine. This would have put LLM inference (seconds) in the search path, defeating sqry's core advantage (graph queries in milliseconds).

### The correct architecture

```
User or AI agent: "find authentication logic"
    |
    v
NL translation layer: converts to sqry query syntax
    |  output: sqry query "kind:function AND name~=auth"
    v
sqry core: executes query via AST/graph (milliseconds)
    |
    v
Results
```

LLMs translate natural language to sqry commands. sqry searches. The translation adds ~1-2 seconds of overhead, but the search itself remains fast.

### Why this matters

If you see references to "embedding-based search" in older documentation or code comments, those describe the **incorrect** architecture that was never shipped. The correct terms are "NL translation" or "natural language interface."

---

## Using the NL Interface

- **CLI**: `sqry ask "your question here"`
- **MCP tool**: `sqry_ask` (translates NL to sqry query, then executes)

See `sqry ask --help` for options.

---

## For AI Agent / MCP Integrators

The NL layer is especially useful for AI agents that use sqry via MCP. Instead of learning sqry's query syntax, agents can call `sqry_ask` with natural language:

```json
{
  "tool": "sqry_ask",
  "params": {
    "query": "find authentication logic"
  }
}
```

The NL layer translates the query, sqry executes it, and results are returned. The agent doesn't need to know sqry syntax.

---

*Version 1.0 - 2025-11-21*
