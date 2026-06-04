# Natural Language Queries

`sqry ask` translates plain English into a validated sqry command. It is a translation and safety layer; immediate execution happens only when `--auto-execute` is supplied and the confidence threshold is satisfied.

## Quick Start

```bash
sqry index .
sqry ask "find authentication functions in rust"
sqry ask --dry-run "who calls authenticate"
sqry ask --auto-execute --threshold 0.90 "find public classes"
```

## CLI Syntax

```text
sqry ask <query> [path] [--auto-execute] [--dry-run] [--threshold <0.0-1.0>] \
  [--model-dir <PATH>] [--allow-unverified-model] [--allow-model-download]
```

| Option | Default | Meaning |
| --- | --- | --- |
| `<query>` | required | Natural-language request to translate. |
| `[path]` | current directory | Optional search path. |
| `--dry-run` | off | Show the generated command without running it. |
| `--auto-execute` | off | Run high-confidence commands without confirmation. |
| `--threshold` | `0.85` | Minimum confidence for auto-execution. |
| `--model-dir` | resolver default | Use a specific classifier model directory containing `manifest.json`. |
| `--allow-unverified-model` | off | Permit a classifier whose checksums cannot be verified. |
| `--allow-model-download` | off | Permit fetching the classifier model when it is not present locally. |

`SQRY_NL_ALLOW_UNVERIFIED_MODEL=1` and `SQRY_NL_ALLOW_DOWNLOAD=1` provide environment-variable equivalents for the two integrity escape hatches.

## Confidence Behavior

| Tier | Confidence | Behavior |
| --- | --- | --- |
| Execute | At or above threshold | Runs only with `--auto-execute`; otherwise asks for confirmation. |
| Confirm | Medium confidence | Shows the command and asks for confirmation. |
| Disambiguate | Low confidence | Presents alternatives or asks for a clearer request. |
| Reject | Unsafe or unsupported | Refuses to generate a command. |

## Generated Command Shapes

Symbol search examples use predicate-in-query syntax:

```bash
sqry ask --dry-run "find authenticate function"
# sqry query "kind:function AND name:authenticate"

sqry ask --dry-run "show public classes"
# sqry query "kind:class AND visibility:public"

sqry ask --dry-run "authentication functions in go"
# sqry query "kind:function AND lang:go AND name:*auth*"
```

Relation examples:

```bash
sqry ask --dry-run "who calls login"
# sqry graph direct-callers login

sqry ask --dry-run "what does main call"
# sqry graph direct-callees main

sqry ask --dry-run "trace from main to database"
# sqry graph trace-path main database
```

Visualization examples use the current positional relation-query form:

```bash
sqry ask --dry-run "visualize callers of authenticate"
# sqry visualize "callers:authenticate" --format mermaid
```

Text-search requests generate search commands:

```bash
sqry ask --dry-run "grep for TODO comments"
# sqry search "TODO"
```

## Safety

Generated commands are validated before execution. Validation rejects unsupported commands, shell metacharacters, path traversal, unsafe write/delete operations, and inputs that fail normalization checks.

`sqry ask` is intended for read-oriented search and analysis workflows. Use explicit sqry commands when you need precise flags or write-capable maintenance operations.

## MCP

MCP exposes the same concept through `sqry_ask`:

```json
{
  "name": "sqry_ask",
  "arguments": {
    "query": "who calls the authenticate function?",
    "path": ".",
    "execute": false
  }
}
```

Set `execute` deliberately. Model directory, download, and unverified-model controls are available as tool parameters in current MCP schemas.
