# Rules

`sqry rules run` executes a shipped rule, a shipped pack, or a workspace TOML pack against the indexed graph. MCP clients use `rules_run`. Cross-snapshot rules report `unsupported` on this single-snapshot path.

## Usage

```bash
sqry rules run bbnty.intake
sqry rules run bbnty.recipes --format json
sqry rules run bbnty.security
sqry rules run bbnty.all
sqry rules run path/to/pack.toml
```

Built-in selectors:

| Selector | Meaning |
| --- | --- |
| `bbnty.recipes` | Shipped bbnty proof-recipe rules |
| `bbnty.intake` | Standard first-run intake pack |
| `bbnty.security` | Universal security detectors |
| `bbnty.all` | Recipes plus intake plus security |

Any other selector is treated as a TOML rule-pack path in the workspace. A selector that matches a shipped rule ID runs that single rule.

`--format text` (default) prints status, an output summary, and a witness summary. `--format json` emits the machine-readable report.

SimilarTo rules run in-engine via structural neighbours (`shape~=` / body-shape). Disable the MCP tool at runtime with `SQRY_MCP_ENABLE_RULES=false` if you need to hide it from a client.
