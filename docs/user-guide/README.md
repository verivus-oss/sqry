# sqry User Guide

**Version**: 23.2.0

This guide covers the stable public workflows for local semantic code search.

## Core Workflows

- [Indexing](indexing.md): build graph artifacts, choose plugins, clean generated state, and rebuild after graph-format changes.
- [Workspaces](workspace.md): configure `.sqry-workspace`, VS Code `.code-workspace` `sqry.workspace`, multi-root status, and workspace cleanup.
- [Daemon Mode](daemon.md): run `sqryd`, load and rebuild workspaces, inspect logs, and use daemon-backed MCP/LSP workflows.
- [Revision-Aware Workspaces](revision-aware-workspaces.md): load immutable revisions, dirty snapshots, and managed agent worktrees while preserving live-workspace query defaults.
- [MCP](mcp.md): configure AI assistants, choose standalone versus daemon mode, inspect the live tool catalog, and handle `source_root_id` safely.
- [Advanced Analysis](advanced-analysis.md): use graph predicates, `resolved_via`, `returns`, context propagation, semantic diff, impact, and visualization.
- [Structural Shape Matching](shape-match.md): find functions by identifier-blind body shape with `shape-match`, `diff --structural`, the `shape~=` predicate, and the `structural_similar` MCP tool.
- [Visualization](visualization.md): render relationship graphs in Mermaid, DOT, and D2.

## First Path

```bash
sqry index .
sqry query "kind:function AND visibility:public"
sqry graph direct-callers authenticate
sqry visualize "callers:authenticate" --format mermaid
```

For installation and a shorter command tour, start with [Quick Start](../../QUICKSTART.md).

## Current Graph Model

sqry writes the current unified graph snapshot format and loads older supported formats through upconversion where available. Binding-plane name-resolution support was introduced historically in V9; current releases write the current format rather than a V9 snapshot.

Use a forced rebuild after upgrading across releases that change graph semantics:

```bash
sqry index --force .
```

## Graph Provenance And Resolution

Use current graph commands instead of missing history pages:

```bash
sqry graph resolve authenticate --explain --json
sqry graph trace-path main handle_request
sqry impact authenticate --depth 3
sqry diff main HEAD
```

These commands expose why graph facts exist and what depends on them without relying on unreleased history pages.

## MCP Response Redaction

For external LLM usage, prefer the `standard` redaction preset unless you need strict path privacy. The MCP runtime default is `minimal`; component docs distinguish runtime defaults from recommended external-provider presets.

See [MCP](mcp.md) and [sqry-mcp/USER_GUIDE.md](../../sqry-mcp/USER_GUIDE.md).

## Index Validation

Check index status before expensive workflows:

```bash
sqry index --status --json .
sqry graph stats --path .
sqry workspace status . --json
```

If graph semantics changed across an upgrade, or if persisted artifacts appear stale, rebuild:

```bash
sqry index --force .
```

## Component Docs

- [CLI README](../../sqry-cli/README.md)
- [MCP README](../../sqry-mcp/README.md)
- [MCP troubleshooting](../../sqry-mcp/TROUBLESHOOTING.md)
- [VS Code extension README](../../sqry-vscode/README.md)
- [VS Code user guide](../../sqry-vscode/USER_GUIDE.md)
