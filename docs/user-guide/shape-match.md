# Structural Shape Matching

This guide covers the body-shape descriptor surfaces: finding functions that share
a control-flow shape regardless of their names, and pairing functions across a diff
by shape rather than by name.

## What a shape descriptor is

For every Function/Method body, sqry computes an identifier-blind structural
fingerprint from the AST (control-flow buckets, structural arities, and a
rename-invariant hash plus a MinHash sketch). It is computed from grammar
structure, never from source text, so renaming every identifier and literal, or
reformatting and adding comments, leaves it unchanged. It is distinct from
`body_hash` (which is exact-bytes and powers `find_duplicates`): two functions with
the same shape but different names and literals share a `shape_hash`, while their
`body_hash` differs.

This answers questions name-based search cannot: "what else is shaped like this
function", and "which function did this renamed-and-moved one used to be".

## sqry shape-match

Find functions structurally similar to a reference function.

```bash
sqry shape-match parse_config
sqry shape-match parse_config --file src/config.rs   # disambiguate overloads
sqry shape-match parse_config --threshold 0.8 --limit 10
sqry shape-match parse_config --json
```

Flags:

- `--file <path>`: restrict the probe to a function defined in this file.
- `--path <path>`: search path (defaults to the current directory).
- `--threshold`, `-t`: minimum MinHash similarity floor, 0.0 to 1.0 (default 0.6).
- `--limit`, `-l`: maximum results (default 20).

Each match reports two numbers:

- `shape_hash_exact`: true when the match's structural hash is byte-identical to the
  probe (an exact rename/relocate-invariant twin).
- `jaccard`: approximate MinHash Jaccard similarity, 0.0 to 1.0.

The lookup is routed through the LSH band index, so it stays sublinear on large
workspaces (it probes the bands the reference shares, then refines a small
candidate set rather than scanning every function).

`sqry shape-match` is distinct from `sqry similar`, which matches by name and fuzzy
text. Use `similar` to find like-named symbols; use `shape-match` to find like-shaped
bodies.

## sqry diff --structural

Pair functions across two git refs by body shape first, then by near-match, so a
function that was renamed or relocated is recognised as the same shape instead of
showing up as one deletion plus one addition.

```bash
sqry diff HEAD~1 HEAD --structural
sqry diff main HEAD --structural --json
```

Exact `shape_hash` pairs are reported first (these survive rename and relocate),
then MinHash near-matches, then the base-only and target-only residue. A pair whose
names differ is flagged as renamed.

## shape~= planner predicate

Structural similarity is a first-class query predicate, composable with the other
planner predicates:

```bash
sqry plan-query "kind:function shape~=parse_config"
sqry query "kind:function AND shape~=parse_config"
```

`shape~=<symbol>` resolves to the structural neighbours of `<symbol>`. Combine it
with `kind:`, `file:`, and the relationship predicates to scope the search.

## MCP structural_similar

AI assistants reach the same capability through the `structural_similar` MCP tool
(daemon-hosted and standalone). It is NodeId-anchored from a strict-resolved start
and returns the same two numbers (`shapeHashExact`, `jaccard`) per neighbour. It is
distinct from the name-based `search_similar` tool. The tool is gated by
`SQRY_MCP_ENABLE_STRUCTURAL_SIMILAR`.

## Coverage and limits

- Every shipped language plugin computes shape descriptors for real function and
  method definitions; descriptors are identifier-blind and deterministic across
  runs and processes.
- Bodies under four tokens carry an explicit `unhashable` marker rather than a
  meaningless fingerprint, and are not returned as structural neighbours.
- `shape_hash` and `minhash` are computed over each language's own grammar, so they
  are not compared across languages; the control-flow histogram is the
  language-neutral surface that is comparable cross-language.
