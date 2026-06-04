# Workspaces

Use a workspace when one logical codebase spans multiple repositories or folders.

## Registry Workflow

```bash
sqry workspace init .
sqry workspace scan .
sqry workspace status . --json
```

The registry records source roots so CLI, MCP, LSP, daemon, and VS Code workflows can resolve the same logical workspace.

Manual root management:

```bash
sqry workspace add /path/to/repo
sqry workspace remove /path/to/repo
sqry workspace stats .
sqry workspace query . "kind:function AND lang:rust"
```

## VS Code Workspace Files

VS Code `.code-workspace` files can contain a `sqry.workspace` block. The extension, LSP, and sqry workspace resolver use that block to identify source roots and aggregate status.

Keep source-root paths normal filesystem paths in configuration. Do not use MCP `source_root_id` values as paths.

## Status

```bash
sqry workspace status . --json
```

Status reports aggregate health for each source root, including whether graph artifacts are present, stale, building, missing, or errored.

## Cleanup

Preview cleanup first:

```bash
sqry workspace clean . --dry-run
```

Then remove stale generated artifacts and rebuild:

```bash
sqry workspace clean .
sqry index --force .
```

## Ignore Rules

Generated graph and cache artifacts should usually stay local:

```gitignore
.sqry/
.sqry-index
.sqry-cache/
```

Commit `.sqry-workspace` only when it is part of your team workflow and contains portable paths.

## MCP Source Roots

MCP `workspace_status` reports `source_root_id` as an opaque display/correlation token. It is useful for matching redacted result paths back to status entries, but it is not a path. Tools that accept `path` arguments require workspace-relative paths or normal filesystem paths.
