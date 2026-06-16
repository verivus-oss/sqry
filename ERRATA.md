# ERRATA: Natural Language Interface Architecture (REMOVED)

**Date**: 2025-11-21 (original errata); 2026-06-14 (removal)
**Status**: Historical / non-current. The natural-language surface described
below (`sqry ask`, the `sqry_ask` MCP tool, the `sqry-nl` crate, the ONNX
classifier and embedding-model artifacts) was removed from sqry. See
`docs/reviews/sqry-nl-removal/2026-06-14/`. Nothing in this file describes a
shipped or supported capability. It is retained only as a record of the design
debate that preceded the removal.

---

## Summary (historical)

Early documentation for sqry's natural language feature incorrectly described an
architecture where LLMs and embeddings would **search** the codebase. The
architecture that was actually built used LLMs only to **translate** natural
language into sqry query syntax, with sqry's core search engine (AST/graph)
handling the actual search.

That translation layer (CLI `sqry ask`, MCP `sqry_ask`, the `sqry-nl` crate) has
since been removed entirely. sqry's supported interface is structured-predicate
search via CLI, LSP, and the MCP tool surface (no natural-language entry point).

---

## What the debate was about (historical)

### The incorrect description

An earlier design described a pipeline where embeddings would search code chunks
and an LLM would rerank results before passing them to sqry's graph engine. This
would have put LLM inference (seconds) in the search path, defeating sqry's core
advantage (graph queries in milliseconds). It was never shipped.

### The translation architecture that did ship (now removed)

```
User or AI agent: "find authentication logic"
    |
    v
NL translation layer: converted to sqry query syntax
    |  output: sqry query "kind:function AND name~=auth"
    v
sqry core: executed query via AST/graph (milliseconds)
    |
    v
Results
```

The translation layer added overhead on the order of one to two seconds per
query. It was removed in 2026-06; structured-predicate queries are now the only
supported interface.

---

*Original errata version 1.0 (2025-11-21); superseded by removal 2026-06-14.*
