# Agent skills

The consumer-facing agent skills for sqry, and the plugin manifests that register
`sqry-mcp` and `sqry-lsp` alongside them. These teach an agent **when to reach for
sqry mid-task**. They are not about developing sqry.

For the contributor skills, the ones that describe working *in this repository*, see
`.claude/skills/` and `skills/`. The two sets share three names (`sqry-claude`,
`sqry-codex`, `sqry-gemini`) and mean opposite things, which is why they live in
separate trees.

## Contents

| Skill | Agent |
|-------|-------|
| `sqry-semantic-search` | all: routing, CLI fallback, disambiguation, output sizing |
| `sqry-claude` | Claude Code |
| `sqry-codex` | OpenAI Codex |
| `sqry-gemini` | Gemini CLI |
| `sqry-grok` | Grok |
| `sqry-opencode` | OpenCode |
| `sqry-antigravity` | Google Antigravity |
| `sqry-mistralvibe` | Mistral Vibe |

`.claude-plugin/plugin.json`, `.mcp.json` and `.lsp.json` make this directory a
Claude-compatible plugin, so a host that discovers it registers the skills, the MCP
server and the LSP server together.

The eight skills live under `skills/`, which is what `plugin.json`'s `"skills": "./skills"`
field names and what a plugin host scans. They were briefly siblings of that manifest
instead, which meant the documented loader would have registered the MCP and LSP servers
and zero skills. Do not flatten them back.

`.mcp.json` here declares the `sqry` server **for consumers of this plugin**. It is not
a repo-root `.mcp.json` and does not shadow the user-scope MCP entries the workspace
convention reserves.

## What belongs here, and what does not

The binary already serves its own reference material over MCP: `sqry://docs/tool-guide`,
`sqry://docs/query-syntax`, `sqry://docs/patterns`, `sqry://docs/architecture`,
`sqry://docs/capability-map` and `sqry://meta/manifest`. Those version with the binary a
user actually installed, so they cannot drift from it.

So the rule for this tree is: **routing stays, reference goes to the resources.** A skill
says when to reach for a tool and what to do when MCP is not connected. It does not
re-document parameters that `tools/list` already returns. That rule is what the
reconciliation applied when these were vendored: the previous in-repo copy carried a
duplicated tool reference, a 22-field query-syntax table and three reference sub-files,
all of which the resources already serve.

## Source of truth

This directory is the source. `verivus-oss/sqry-skills` is a publish target, not an
upstream. Edit here.

Consumed by:

- `benchmarks/swebench/prepare.sh`, which stages per-agent skills into the benchmark image
- `.claude/skills/sqry-semantic-search/SKILL.md`, a copy so this repository's own agents
  load the same routing skill, kept honest by `scripts/ci/check_agent_skills_parity.py`

That second one is a copy rather than a symlink because
`scripts/release/sanitize-for-oss.sh` scans the whole tree for symlinks and fails closed
on the first one, before it applies the public allowlist. One symlink anywhere stops the
release being built at all.
