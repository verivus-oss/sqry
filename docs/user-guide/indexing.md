# Indexing

sqry indexes source into `.sqry/` artifacts so search, graph analysis, MCP, LSP, and daemon workflows can reuse the same graph.

## Build And Inspect

```bash
sqry index .
sqry index --status --json .
sqry graph stats --path .
```

Use `--force` after upgrading across releases that changed graph semantics or when you intentionally want a clean rebuild:

```bash
sqry index --force .
```

## Artifacts

Common generated paths:

- `.sqry/graph/snapshot.sqry`: persisted unified graph snapshot.
- `.sqry/graph/manifest.json`: graph identity, plugin selection, and metadata.
- `.sqry/analysis/`: derived analysis artifacts when present.
- `.sqry/graph/derived.sqry`: daemon/analysis derived-cache artifact when present.
- Legacy `.sqry-index`: older symbol-index marker used by compatibility paths.

Add generated sqry artifacts to `.gitignore` unless your team has an explicit policy for sharing them:

```gitignore
.sqry/
.sqry-index
.sqry-cache/
```

## Plugin Selection

The default index path uses fast plugins. sqry persists the active plugin set in the graph manifest so later commands do not reinterpret the workspace with different plugin defaults.

```bash
sqry index .                         # default fast path
sqry index --include-high-cost .     # all compiled non-default plugins
sqry index --exclude-high-cost .     # force the fast path
sqry index --enable-plugin json .    # enable one plugin
sqry index --disable-plugin json .   # disable one plugin
```

`json` is a high-wall-clock plugin. Optional specialty plugins are available only when compiled into the registry: `apex`, `abap`, `servicenow-xanadu-js`, `servicenow-xml`, `terraform`, `puppet`, and `pulumi`.

Build with specialty plugins when those languages are part of your public workflow:

```bash
cargo build -p sqry-cli --features specialty-plugins
```

## Cleanup

Preview stale generated state (dry-run is the default):

```bash
sqry workspace clean .
```

Remove artifacts only when you are ready to rebuild. Deletion requires `--apply`:

```bash
sqry workspace clean . --apply
sqry index --force .
```

## Troubleshooting

- If `sqry query` reports missing graph state, run `sqry index .`.
- If a command loads older graph semantics after an upgrade, run `sqry index --force .`.
- If optional plugin ids are missing from a manifest, rebuild with the same feature set that produced the original graph or rebuild the index with the current binary.
