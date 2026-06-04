# Homebrew Tap Auto-Publishing

The Homebrew tap [`verivus-oss/homebrew-sqry`](https://github.com/verivus-oss/homebrew-sqry)
is auto-published on every GitHub release of `verivus-oss/sqry`. End users always
get the latest formula via `brew upgrade sqry` with zero manual intervention.

## Components

| File | Role |
|------|------|
| `packaging/homebrew/sqry.rb` | Canonical formula template with `@PLACEHOLDER@` tokens for version, repo, and 16 SHA256 checksums. |
| `scripts/release/render-homebrew-formula.sh` | Downloads `SHA256SUMS.txt` for a given release tag, substitutes placeholders, emits a publish-ready `Formula/sqry.rb`. |
| `.github/workflows/publish-homebrew-tap.yml` | Triggered by every `release: published` event (and on `workflow_dispatch`); renders the formula and pushes it to the tap. |

## How auto-publishing works

1. `release-plz-release.yml` (or any workflow that creates a GitHub release) publishes a new release on `verivus-oss/sqry`.
2. The `release: { types: [published] }` trigger on `publish-homebrew-tap.yml` fires.
3. The workflow:
   - Resolves the target tag from `github.event.release.tag_name`.
   - Skips if the release is `draft` or `prerelease` (early exit, no commit).
   - Checks out the source repo, installs Ruby for syntax validation.
   - Runs `scripts/release/render-homebrew-formula.sh --tag <tag>` to produce `Formula/sqry.rb`.
   - Validates the rendered formula with `ruby -c`.
   - Checks out `verivus-oss/homebrew-sqry@main` using the `HOMEBREW_TAP_PUSH_TOKEN` secret.
   - Copies the rendered formula in place; on first run (existing README < 1 KB) also installs a real README.
   - Commits as `verivusOSS-releases <releases@sqry.dev>` with message `chore(formula): bump to <tag>` and pushes to `main`.

The job uses `concurrency: { group: publish-homebrew-tap, cancel-in-progress: false }` so two near-simultaneous releases publish in order rather than racing.

## Required secret: `HOMEBREW_TAP_PUSH_TOKEN`

The workflow needs a Personal Access Token with `contents:write` permission on `verivus-oss/homebrew-sqry`.

### Creating the token

1. Visit <https://github.com/settings/personal-access-tokens/new>.
2. **Resource owner**: `verivus-oss`.
3. **Repository access**: Only select repositories → `verivus-oss/homebrew-sqry`.
4. **Repository permissions** → set **Contents** to **Read and write**. Leave everything else at no-access.
5. Set an expiration that matches your rotation policy (e.g. 1 year). Generate the token.

### Installing the token

In `verivus-oss/sqry`:

1. Settings → Secrets and variables → Actions → New repository secret.
2. **Name**: `HOMEBREW_TAP_PUSH_TOKEN`.
3. **Secret**: paste the PAT.
4. Save.

The next release publish (or manual `workflow_dispatch`) will pick it up.

## Manual publish via `workflow_dispatch`

Use this to republish a stale tap, backfill a historical tag, or recover from a failed auto-publish.

### From the GitHub UI

1. Go to Actions → "Publish Homebrew Tap".
2. Click **Run workflow**.
3. Optionally set `tag` to a specific `vX.Y.Z` (defaults to the latest non-draft, non-prerelease release).
4. Click **Run workflow**.

### From the CLI

```bash
# Latest release
gh workflow run publish-homebrew-tap.yml --repo verivus-oss/sqry

# Specific tag
gh workflow run publish-homebrew-tap.yml --repo verivus-oss/sqry -f tag=v9.0.23
```

## Backfilling an older release

The workflow accepts any conforming tag (`vMAJOR.MINOR.PATCH[-prerelease]`) for which a GitHub release exists with a `SHA256SUMS.txt` asset. If you need to roll back the tap to an older release, dispatch the workflow with that tag — the resulting commit message will read `chore(formula): bump to <older-tag>`.

## Local rendering (for testing)

```bash
bash scripts/release/render-homebrew-formula.sh --tag v9.0.23 --out /tmp/sqry.rb
```

Requires `gh` (preferred) or `curl` plus `python3`. The script downloads the release's `SHA256SUMS.txt`, parses the 16 brew-relevant checksums (`sqry`, `sqry-mcp`, `sqry-lsp`, `sqryd` × `linux-arm64`, `linux-x86_64`, `macos-arm64`, `macos-x86_64`), substitutes 19 placeholders, and audits the output for any remaining `@...@` tokens.

If `brew` is on `PATH` the script also runs `brew style` as a best-effort lint (warnings non-fatal).

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| Workflow fails at "Render formula" with "missing SHA256 for asset 'sqryd-…'" | Release is missing a binary asset (e.g. partial upload). | Re-upload the missing asset, then redispatch. |
| Workflow fails at "Checkout tap repo" with 403/404. | `HOMEBREW_TAP_PUSH_TOKEN` is missing, expired, or lacks `contents:write` on the tap repo. | Recreate the token per "Creating the token" above and update the secret. |
| Workflow succeeds but commit says "No diff in tap repo". | Tag was already published — formula bytes match. | No action needed. |
| Two releases land seconds apart and the second's run "queues". | `concurrency.cancel-in-progress: false` is intentional — runs serialize. | Wait; both will publish in order. |

## Relationship to other release workflows

The tap publish runs **after** the GitHub release exists (because that's its trigger). It is decoupled from `release-plz-release.yml`, `oss-sanitize-publish.yml`, and `release-get-well-direct.yml`. Those workflows produce the release; this workflow ships the brew artefact derived from it. Adding more publish targets (Scoop, Snap, AUR) should follow the same pattern: separate workflow, release-event trigger, dedicated PAT secret.

## Public package scope

Homebrew is the package-manager surface currently selected by
`release-manifest.toml` for public publication. Do not document another package
manager as public-ready until its template, checksum renderer, release-event
workflow, component coverage, and public validation are complete.

The formula must continue to cover `sqry`, `sqry-mcp`, `sqry-lsp`, and `sqryd`
for Linux `x86_64`/`arm64` and macOS `x86_64`/`arm64`.

Run the public docs drift check during release maintenance:

```bash
scripts/check_public_docs_drift.sh
```
