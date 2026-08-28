# Repository Overview

`sqry overview` (alias `sqry report`) produces a one-shot orientation map of the indexed workspace: summary, load-bearing hubs, path/package subsystems, complexity hotspots, potential issues, and follow-up queries.

MCP clients use the matching `generate_overview` tool. That tool is in the daemon-hosted 17-tool subset.

## Usage

```bash
sqry overview
sqry overview --format json
sqry overview --sections hubs,issues
sqry overview --top 5 --output MAP.md
```

| Flag | Meaning |
| --- | --- |
| `--format md\|json\|text` | Markdown report (default), JSON, or a terse digest |
| `--sections` | Comma-separated subset: `summary`, `hubs`, `subsystems`, `hotspots`, `issues`, `questions` |
| `--top N` | Maximum rows per ranked section (default 10) |
| `--group-depth N` | Leading path components used as a subsystem bucket (default 2) |
| `--output FILE` | Write the report to a file instead of stdout |
| `--no-index` | Fail if the graph is missing or stale instead of building it |
| `--redaction minimal\|none\|relative` | Path/name redaction (default `minimal`) |

The same ranked views are also available as graph subcommands:

```bash
sqry graph hubs
sqry graph subsystems
sqry graph communities
```

Index first (`sqry index .`) on a cold workspace. Then run `sqry overview` and follow the suggested queries rather than starting from a blank `sqry query`.
