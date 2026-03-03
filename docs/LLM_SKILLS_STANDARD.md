# LLM Skills Standard

**Status**: Active  
**Last Reviewed**: 2026-02-20  
**Applies To**: Codex CLI, Claude Code, Gemini CLI workflows in this repository

## Purpose

Define a single, repo-wide baseline for:
- what "context files" are
- what "skills" are
- how capabilities and metadata differ across Codex, Claude, and Gemini

## Core Definitions

### Persistent Context Files

Always-loaded or frequently-loaded instruction files that set repository policy and workflow.

In this repository:
- `AGENTS.md` (entrypoint index)
- `CODEX.md` (Codex-focused guidance)
- `CLAUDE.md` (Claude-focused guidance)
- `GEMINI.md` (Gemini-focused guidance)

### Skills (On-Demand Expertise)

A skill is a reusable, activatable capability packaged as a directory with a required `SKILL.md` file.

Required metadata:
- `name`
- `description`

Recommended structure:
- `SKILL.md`
- `scripts/` (deterministic automation)
- `references/` (large docs loaded only when needed)
- `assets/` (templates/artifacts)

### UI Metadata (Codex/OpenAI)

`agents/openai.yaml` is optional metadata for OpenAI/Codex skill UIs (display name, short description, default prompt).  
It is not required for skill behavior and has no Claude/Gemini equivalent.

## Capability Matrix

| Agent | Persistent context | On-demand skills | Skill UI metadata |
|---|---|---|---|
| Codex CLI | `AGENTS.md`, `CODEX.md` | `SKILL.md`-based skills | Optional `agents/openai.yaml` |
| Claude Code | `CLAUDE.md` (+ `.claude/*` memory/rules) | `.claude/skills/*/SKILL.md` | No equivalent to `openai.yaml` |
| Gemini CLI | `GEMINI.md` (default), optional `context.fileName` list | `.gemini/skills/*/SKILL.md` | No equivalent to `openai.yaml` |

## Authoring Rules

- Keep `description` explicit about when the skill should be used.
- Keep `SKILL.md` concise; move heavy details to `references/` or scripts.
- Treat skills as code: review diffs, avoid secrets, keep behavior auditable.
- Side-effect workflows (deploy/publish/destructive actions) must require explicit user intent.
- Do not duplicate stable repository policy in every skill; keep shared policy in context files.

## Repository Mapping

- Agent entrypoint: `AGENTS.md`
- Agent guides: `CODEX.md`, `CLAUDE.md`, `GEMINI.md`
- Agent-native project skills:
  - `.claude/skills/sqry-repo/SKILL.md`
  - `.gemini/skills/sqry-repo/SKILL.md`
- Gemini project config:
  - `.gemini/settings.json` (loads `AGENTS.md` + `GEMINI.md` via `context.fileName`)
- Repo-local skill wrappers:
  - `skills/sqry-codex/SKILL.md`
  - `skills/sqry-claude/SKILL.md`
  - `skills/sqry-gemini/SKILL.md`
- Codex/OpenAI UI metadata:
  - `skills/sqry-codex/agents/openai.yaml`
  - `skills/sqry-claude/agents/openai.yaml`
  - `skills/sqry-gemini/agents/openai.yaml`

## Gemini Context Filename Example

Gemini defaults to `GEMINI.md`. To also load `AGENTS.md`, configure:

```json
{
  "context": {
    "fileName": ["AGENTS.md", "GEMINI.md"]
  }
}
```

## References (Primary Sources)

- Anthropic Claude Code skills docs: `https://docs.anthropic.com/en/docs/claude-code/skills`
- Anthropic Claude Code best practices: `https://docs.anthropic.com/en/docs/claude-code/best-practices`
- Claude API Agent Skills overview: `https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview`
- Claude API Agent Skills best practices: `https://platform.claude.com/docs/en/agents-and-tools/agent-skills/best-practices`
- Gemini CLI context files: `https://google-gemini.github.io/gemini-cli/docs/cli/gemini-md.html`
- Gemini CLI configuration: `https://google-gemini.github.io/gemini-cli/docs/get-started/configuration.html`
- Gemini CLI skills: `https://geminicli.com/docs/cli/skills/`
- Gemini CLI creating skills: `https://geminicli.com/docs/cli/creating-skills/`
