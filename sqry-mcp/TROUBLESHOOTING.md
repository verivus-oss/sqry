# sqry MCP Troubleshooting

**Version**: 27.0.6

## Confirm The Binary

```bash
sqry-mcp --version
sqry-mcp --list-tools
```

Standalone mode currently lists 37 tools. Daemon-hosted MCP currently exposes a 16-tool subset. If the count differs, trust the live output and update local docs or client expectations.

## MCP Client Does Not See Tools

1. Confirm `sqry-mcp` is on `PATH`.
2. Run `sqry-mcp --list-tools` in the same shell environment.
3. Re-run setup:

```bash
sqry mcp setup --tool claude --dry-run
sqry mcp setup --tool codex --dry-run
sqry mcp setup --tool gemini --dry-run
```

4. Restart the MCP client so it reloads configuration.

Codex and Gemini use global config and resolve the workspace from where the assistant is launched.

## Standalone Versus Daemon Confusion

Standalone:

```bash
sqry-mcp --no-daemon
```

Daemon:

```bash
sqry daemon start
sqry daemon load .
sqry-mcp --daemon
```

Daemon-hosted MCP exposes a smaller daemon-backed subset. Use standalone mode when you need the full catalog.

## No Index Found

Build or inspect the index:

```bash
sqry index .
sqry index --status --json .
```

For workspaces:

```bash
sqry workspace status . --json
```

## Stale Or Incompatible Graph

Run a forced rebuild:

```bash
sqry index --force .
```

If daemon state is also involved:

```bash
sqry daemon reset .
sqry daemon load .
```

Current releases write the current snapshot format and load supported older formats through upconversion. Avoid assuming a historical format name describes the current writer.

## `source_root_id` Used As A Path

If a tool call fails with a path shaped like this:

```text
485f1995/src/lib.rs
```

replace it with a workspace-relative or absolute path:

```text
src/lib.rs
/absolute/path/to/workspace/src/lib.rs
```

`source_root_id` is only an opaque display/correlation token.

## Redaction Unexpected

The runtime default is `minimal`. For external providers, use `standard` unless you need `strict`.

If you need cleartext source-root paths for local diagnostics, use a preset that permits the top-level `source_roots[]` carrier. Do not convert `source_root_id` into tool path arguments.

## More Help

- [MCP guide](../docs/user-guide/mcp.md)
- [Workspace guide](../docs/user-guide/workspace.md)
- [Daemon guide](../docs/user-guide/daemon.md)
- [sqry-mcp README](README.md)
