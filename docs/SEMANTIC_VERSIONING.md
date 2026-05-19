# Semantic Versioning Guide

**Version**: 16.0.2
**Last Updated**: 2026-05-20

---

## Version Scheme

sqry follows [Semantic Versioning](https://semver.org/) in the form `MAJOR.MINOR.PATCH`.

- **MAJOR** — Breaking API or workflow changes
- **MINOR** — Backwards-compatible feature additions
- **PATCH** — Bug fixes or performance improvements that preserve behaviour

Starting from `4.0.0`, the project follows strict semver. Breaking changes require a MAJOR bump with migration documentation.

All crates in the workspace share a single version number managed in `Cargo.toml` under `[workspace.package]`.

---

## Conventional Commit Mapping

sqry uses [Conventional Commits](https://www.conventionalcommits.org/) to drive versioning and changelog generation.

| Commit prefix | Typical change type | Version bump |
|---------------|---------------------|--------------|
| `feat:` | New user-visible capability | MINOR |
| `fix:` | Bug fix | PATCH |
| `perf:` | Performance improvement | PATCH |
| `refactor:` | Internal refactor without behaviour change | None (unless `BREAKING CHANGE`) |
| `docs:` / `test:` / `chore:` / `ci:` | Documentation, tests, tooling | None |
| `BREAKING CHANGE:` footer | Explicit breaking change | MAJOR |

Use scopes to indicate the affected component (e.g., `feat(graph): ...`, `fix(mcp): ...`, `chore(release): ...`). This improves changelog readability and commit traceability.

### Common Scopes

| Scope | Component |
|-------|-----------|
| `core` | sqry-core library |
| `cli` | sqry CLI binary |
| `mcp` | MCP server |
| `lsp` | LSP server |
| `graph` | Unified graph subsystem (within sqry-core) |
| `lang-*` | Language plugins (e.g., `lang-rust`, `lang-python`) |
| `nl` | Natural language query translation (sqry-nl) |
| `vscode` | VS Code extension |
| `release` | Release pipeline and packaging |
| `uses` | Share/export feature |

Scopes not listed here (e.g., `plugin-registry`, `mcp-redaction`, `test-support`) follow the same `crate-name` convention.

---

## Changelog

sqry's changelog follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/):

```markdown
# Changelog

## [Unreleased]

## [16.0.2] - 2026-03-03
### Fixed
- Summary of bug fixes

## [4.8.0] - 2026-03-01
### Added
- Summary of new features

### Changed
- Notable behaviour changes
```

Guidelines:
- Work from the most recent release backwards.
- Group entries by feature scope to mirror conventional commit scopes.
- Reference relevant documentation when useful.
- Note any migration guidance for breaking changes.

---

## Release Checklist

1. Confirm planned version bump (based on recent commits).
2. Update `Cargo.toml` workspace version: `[workspace.package] version = "X.Y.Z"`.
3. Run `cargo generate-lockfile` to update `Cargo.lock`.
4. Ensure `CHANGELOG.md` captures user-facing changes.
5. Validate tests: `cargo test --workspace`.
6. Run `cargo fmt --all` and `cargo clippy --all-targets --workspace -- -D warnings`.
7. Create and push version tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
8. Announce via release notes with highlights from the changelog.

---

## Version History

| Version | Date | Changes |
|---------|------|---------|
| 16.0.2 | 2026-03-03 | Full rewrite; align with current release process |
| 4.5.11 | 2026-02-27 | Initial draft |
