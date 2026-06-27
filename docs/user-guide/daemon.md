# Daemon Mode

`sqryd` keeps graph state warm for repeated CLI, LSP, MCP, and editor workflows.

## Start And Load

```bash
sqry daemon start
sqry daemon load .
sqry daemon status --json
```

`sqry daemon load` always loads the live workspace. Revision-aware operations
are additive; loading a ref, commit, tree, dirty snapshot, or managed worktree
does not change the default for queries that omit a revision selector.

Stop the daemon when you no longer need shared graph state:

```bash
sqry daemon stop
```

## Rebuild

Trigger an in-place rebuild for a loaded workspace:

```bash
sqry daemon rebuild . --force
```

After rebuild, validate persistent state:

```bash
sqry index --status --json .
sqry graph stats --path .
```

Current daemon rebuilds are expected to keep durable graph artifacts coherent with the in-memory graph. If persisted graph state appears stale, run `sqry index --force .` and reload the workspace.

## Revision-Aware Loads

Use revision-aware commands when a daemon must keep more than the live checkout
addressable:

```bash
sqry daemon load-revision . --ref main
sqry daemon load-revision . --dirty --include-untracked
sqry daemon list-revisions --root .
sqry daemon revision-status <revision-id> --json
sqry daemon unload-revision <revision-id>
sqry daemon prune-revisions --root . --json
```

Immutable revisions prefer raw Git object traversal and never fetch missing
objects implicitly. Dirty snapshots hash exact included bytes and fail with
`DirtySnapshotChanged` if content changes during capture after the allowed
retry. Managed worktrees are daemon-owned fallback or agent infrastructure, not
the default immutable source mode.

See [Revision-Aware Workspaces](revision-aware-workspaces.md) for selector
semantics, query provenance, cleanup, and multi-agent rules.

## Logs

```bash
sqry daemon logs
sqry daemon logs --follow
```

Logs are the first place to inspect workspace load failures, rebuild errors, memory pressure, and cache behavior.

## MCP And LSP

Standalone MCP:

```bash
sqry-mcp --no-daemon
```

Daemon-backed MCP:

```bash
sqry-mcp --daemon
```

LSP can also use daemon-backed graph state when configured by the editor or launched with daemon support.

## Recovery

Use this sequence when daemon and disk state disagree:

```bash
sqry daemon status --json
sqry daemon reset .
sqry index --force .
sqry daemon load .
sqry daemon status --json
```

If the daemon is not needed for the workflow, use the in-process CLI path with `sqry index .` and direct commands.

For revision artifacts, run `sqry daemon prune-revisions` first; dry-run is
the default until `--apply` is passed. Startup recovery removes partial
temporary revision artifacts and reconciles daemon-managed worktrees without
force-removing user-created worktrees outside the managed registry.
