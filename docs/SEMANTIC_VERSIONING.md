# Semantic Versioning Guide

🔒 **Status**: Draft (to be referenced by AGENTS.md and DEVELOPMENT_PROCESS.md)
🏷️ **Release tag**: v4.0.0

---

## Version Scheme

sqry follows [Semantic Versioning](https://semver.org/) in the form `MAJOR.MINOR.PATCH`.

- **MAJOR** – Breaking API or workflow changes  
- **MINOR** – Backwards compatible feature additions  
- **PATCH** – Bug fixes or performance improvements that preserve behaviour

While the project remains in the `0.x` range, MINOR releases may include carefully documented breaking changes. Treat `0.MINOR.PATCH` as the primary compatibility signal.

---

## Conventional Commit Mapping

| Commit prefix | Typical change type | Version bump |
|---------------|---------------------|--------------|
| `feat:` | New user-visible capability | MINOR |
| `fix:` | Bug fix | PATCH |
| `perf:` | Performance improvement | PATCH |
| `refactor:` | Internal refactor without behaviour change | None (unless `BREAKING CHANGE`) |
| `docs:` / `test:` / `chore:` / `ci:` | Documentation, tests, tooling | None |
| `BREAKING CHANGE:` footer | Explicit breaking change | MAJOR (or MINOR while `<1.0.0`) |

Use scopes (e.g., `feat(symbols): ...`) to make changelog generation meaningful and keep commit history traceable.

---

## Automation Workflow

Preferred tooling: `release-plz` (CI-driven) or `cargo-release` (local) to ensure consistent version updates and changelog generation.

```yaml
# .github/workflows/release.yml (example)
name: Release

on:
  push:
    branches: [main, master]

jobs:
  release:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: MarcoIeni/release-plz-action@v0.5
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

Automation steps:
1. Parse recent conventional commits.  
2. Determine required version bump.  
3. Update crate versions (`Cargo.toml`, workspace metadata).  
4. Regenerate `CHANGELOG.md`.  
5. Tag the release and publish (if configured).

---

## Manual Version Management

When automation is unavailable, use `cargo-workspaces` helpers:

```bash
# Patch bump (0.2.0 → 0.2.1)
cargo ws version patch

# Minor bump (0.2.1 → 0.3.0)
cargo ws version minor

# Major bump (0.3.0 → 1.0.0)
cargo ws version major
```

Always run `cargo fmt`, `cargo test --workspace`, and update `CHANGELOG.md` before tagging the release.

---

## Changelog Expectations

sqry’s changelog follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/):

```markdown
# Changelog

## [Unreleased]

## [0.2.0] - YYYY-MM-DD
### Added
- Summary of new features

### Fixed
- Summary of bug fixes

### Changed
- Notable behaviour changes
```

Key guidelines:
- Work from the most recent release backwards.  
- Group entries by feature scope to mirror conventional commit scopes.  
- Reference relevant documentation or test plan sections when useful.  
- Note any migration guidance for breaking changes.

---

## Release Checklist

1. Confirm planned version bump (based on recent commits).  
2. Ensure `CHANGELOG.md` captures user-facing changes.  
3. Validate tests: `cargo test --workspace`.  
4. Run `cargo fmt --all` and `cargo clippy --all-targets -- -D warnings`.  
5. Tag release and push (automation handles this when configured).  
6. Announce via release notes with highlights from the changelog.
