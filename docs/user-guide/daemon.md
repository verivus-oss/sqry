# Daemon Mode

`sqryd` keeps graph state warm for repeated CLI, LSP, MCP, and editor workflows.

## Start And Load

```bash
sqry daemon start
sqry daemon load .
sqry daemon status --json
```

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
