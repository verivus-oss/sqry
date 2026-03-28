# Natural Language Queries

sqry supports natural language queries through `sqry ask`, letting you search
and navigate code using plain English instead of structured query syntax.

```bash
sqry ask "find authentication functions in rust"
# → sqry query "name~=/auth/ AND kind:function" --language rust
```

## How It Works

When you type a natural language query, sqry runs it through a translation
pipeline that:

1. **Normalizes** the input (Unicode, homoglyph detection)
2. **Extracts** entities (symbol names, languages, kinds, paths)
3. **Classifies** the intent (what kind of operation you want)
4. **Assembles** a safe sqry command from the extracted entities
5. **Validates** the command against safety rules (whitelist, no shell injection)
6. **Caches** the result for fast repeated queries

The classifier uses a compact ML model (22M parameters, ~57MB) that runs
locally with no network calls. A rule-based fallback is available when the
model is not installed.

## Quick Start

```bash
# Index your codebase first (required for graph commands)
sqry index .

# Ask in plain English
sqry ask "find login function"
sqry ask "who calls authenticate"
sqry ask "trace from main to database"
sqry ask "grep for TODO comments"
```

## Command Reference

```
sqry ask <query> [path] [--auto-execute] [--dry-run] [--threshold <0.0-1.0>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `<query>` | required | Your natural language query |
| `[path]` | current dir | Scope the search to a specific path |
| `--auto-execute` | off | Run high-confidence commands without asking |
| `--dry-run` | off | Show the translated command without running it |
| `--threshold` | `0.85` | Minimum confidence for auto-execution |
| `--json` | off | Output structured JSON instead of interactive text |

## Confidence Tiers

sqry uses confidence scores to decide how to handle each translation:

| Tier | Confidence | Behavior |
|------|------------|----------|
| **Execute** | >= 85% | Shows the command and asks to run it (or runs it with `--auto-execute`) |
| **Confirm** | 65-84% | Shows the command with a confirmation prompt |
| **Disambiguate** | < 65% | Presents multiple options to choose from |
| **Reject** | n/a | Input failed validation; shows reason and suggestions |

### Examples

**Execute tier** (high confidence):
```
$ sqry ask "find login function"
Generated command: sqry query "login" --kind function
Confidence: 92%. Execute? [y/N]
```

**Confirm tier** (medium confidence):
```
$ sqry ask "show authentication stuff"
I'll run: sqry query "authentication"
Confidence: 72%. Proceed? [y/N]
```

**Disambiguate tier** (low confidence):
```
$ sqry ask "authentication"
I'm not sure what you mean. Did you want to:
  1. Search for symbol: sqry query "authentication"
  2. Text search: sqry search "authentication"
Enter choice (1-2) or 'c' to cancel:
```

## Supported Query Types

### Symbol Search

Find functions, classes, structs, and other symbols by name or kind.

```bash
sqry ask "find authenticate function"
sqry ask "where is UserAuth defined"
sqry ask "show me all public classes"
sqry ask "find Config struct"
sqry ask "list all traits"
sqry ask "find login method in rust"
sqry ask "authentication functions in go"
```

Generated commands use `sqry query` with appropriate filters:
```
sqry query "authenticate" --kind function
sqry query "UserAuth"
sqry query "kind:class AND visibility:public"
sqry query "Config" --kind struct
```

### Text Search

Search for patterns in source code (like grep).

```bash
sqry ask "grep for TODO comments"
sqry ask "search for error messages"
sqry ask "find all panic! calls"
sqry ask "grep for hardcoded passwords"
sqry ask "search for deprecated annotations"
```

Generated commands use `sqry search`:
```
sqry search "TODO"
sqry search "panic!"
```

### Find Callers

Discover which functions call a given symbol.

```bash
sqry ask "who calls login"
sqry ask "what calls the save function"
sqry ask "callers of encrypt"
sqry ask "find usages of parse_json"
sqry ask "where is format_output used"
```

Generated commands use `sqry graph direct-callers`:
```
sqry graph direct-callers "login"
sqry graph direct-callers "save"
```

### Find Callees

Discover what a function calls.

```bash
sqry ask "what does main call"
sqry ask "callees of run"
sqry ask "show outgoing calls from validate"
sqry ask "functions called by process_request"
```

Generated commands use `sqry graph direct-callees`:
```
sqry graph direct-callees "main"
sqry graph direct-callees "validate"
```

### Trace Path

Find call paths between two symbols.

```bash
sqry ask "trace from main to database"
sqry ask "path from api to storage"
sqry ask "call chain from bootstrap to run"
sqry ask "how does parse reach execute"
```

Generated commands use `sqry graph trace-path`:
```
sqry graph trace-path "main" "database"
sqry graph trace-path "api" "storage"
```

### Visualize

Generate call graph diagrams.

```bash
sqry ask "visualize auth flow"
sqry ask "draw call graph for login"
sqry ask "show mermaid diagram"
sqry ask "create DOT graph of imports"
```

Generated commands use `sqry visualize`:
```
sqry visualize --relation call --symbol "auth" --format mermaid
```

### Index Status

Check whether the index is up to date.

```bash
sqry ask "index status"
sqry ask "is the index up to date"
sqry ask "how many symbols are indexed"
sqry ask "what files are indexed"
```

Generated commands use `sqry index --status`:
```
sqry index --status
sqry index --status --json
```

## Safety

Every generated command is validated before it can run. The following checks
are applied:

| Check | What it prevents |
|-------|------------------|
| **Command whitelist** | Only known sqry commands are allowed |
| **Shell metacharacters** | Rejects `;` `|` `&` `` ` `` `$()` and other injection vectors |
| **Environment variables** | Rejects `$HOME`, `${VAR}`, etc. |
| **Path traversal** | Rejects `../` to prevent directory escape |
| **Write operations** | Rejects `--force`, `--delete`, `repair`, `prune` |
| **Length limit** | Commands capped at 4KB |
| **Unicode normalization** | Homoglyph attacks (Cyrillic 'a' vs Latin 'a') are detected |

sqry ask generates **read-only** commands. It cannot modify your codebase,
delete files, or execute arbitrary shell commands.

## MCP Integration

AI assistants (Claude, Cursor, etc.) can use sqry's natural language
capabilities through the `sqry_ask` MCP tool:

```json
{
  "name": "sqry_ask",
  "arguments": {
    "query": "who calls the authenticate function?",
    "path": ".",
    "execute": true
  }
}
```

When `execute` is `true`, the tool runs the translated command and returns the
results directly. The MCP tool applies the same safety validation as the CLI,
plus additional workspace-boundary checks that prevent path traversal outside
the project root.

The tool can be disabled by setting `SQRY_MCP_ENABLE_SQRY_ASK=false`.

## JSON Output

Use `--json` for structured output suitable for scripts and integrations:

```bash
sqry ask --json "find login function"
```

```json
{
  "type": "execute",
  "command": "sqry query \"login\" --kind function",
  "confidence": 0.92,
  "intent": "symbol_query"
}
```

All four response tiers produce structured JSON with the `type` field
indicating the tier (`execute`, `confirm`, `disambiguate`, `reject`).

## Tips

- **Be specific**: "find login function in rust" works better than "login"
- **Use `--dry-run`** to see what command would run without executing it
- **Use `--auto-execute`** in scripts where you trust the translation
- **Adjust `--threshold`** to control how confident sqry must be before
  suggesting auto-execution (lower = more permissive)
- **Index first**: Graph commands (`who calls`, `trace path`) require
  `sqry index` to have been run
- **Language filters**: Mention the language ("in rust", "in python") to
  narrow results
- **Quoted symbols**: Use quotes for exact names — `find "processData"`
  preserves casing
