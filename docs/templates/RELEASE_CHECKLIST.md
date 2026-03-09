# Release Checklist

This template enumerates the manual verification steps required before and after publishing a sqry release.

## Pre-Release: Pipeline Legs

### Leg 1 — Sanitize and Stage
- [ ] All CI checks pass on `master` (including `cargo vet`)
- [ ] Version bumped in workspace `Cargo.toml` and `Cargo.lock` updated
- [ ] Release tag created and pushed: `git tag vX.Y.Z && git push origin master vX.Y.Z`
- [ ] Run local preflight: `./scripts/release/preflight-oss-release.sh vX.Y.Z`
- [ ] Trigger the private Leg 1 sanitize workflow for `vX.Y.Z`
- [ ] Confirm Job 1 (sanitize/build/test/scan) passes
- [ ] Approve environment gate `oss-staging` to allow Job 2 push

### Leg 1.5 — Template Consistency Gate (if templates changed)
- [ ] `docs/templates/README.md` includes every added/renamed template file.
- [ ] Template filenames use `_SLUG_` where applicable and examples reference slugged filenames.
- [ ] Updated templates keep `Token Optimization` guidance block.
- [ ] Updated templates contain a current metadata date where present (`Last Updated` / history table).
- [ ] Run duplicate-review guard: `rg -n "Submit for review to all the available llm providers" docs/templates/*.md` and confirm wording is intentional (no accidental repetition drift).
- [ ] For language work: confirm `PLUGIN_TEMPLATE-_SLUG_.md` and `LANGUAGE_FEASIBILITY_GATE.md` are aligned (capabilities + scoring + polyglot tracing).
- [ ] For MCP work: confirm `MCP_INTEGRATION_TEMPLATE-_SLUG_.md` is referenced in template docs and is consistent with CLI/MCP process docs.

### Leg 2 — Sync to Public
- [ ] Human-eyeball the sanitized staging repository and verify no internal org references or internal absolute paths remain
- [ ] Obtain the staging SHA and tree hash from the Leg 1 job summary
- [ ] Trigger the private Leg 2 publish workflow with the staging SHA and tree hash for `vX.Y.Z`
- [ ] Confirm Job 1 (verify/build/test/org-replace) passes
- [ ] Approve environment gate `oss-public` to allow Job 2 push
- [ ] Confirm `verivus-oss/sqry` has the tag and `.github/workflows/oss-leg3-release.yml` is present

### Leg 3 — Build, Sign, and Release
- [ ] Dispatch from `verivus-oss/sqry` Actions UI: Actions → OSS Leg 3 Release → Run workflow → **select the tag ref**
- [ ] Set `enable_signpath` (`true` when SignPath is configured for Windows signing; otherwise `false`).
- [ ] Set `enable_apple_signing` (`true` when Apple Developer ID is configured for macOS; otherwise `false`).
- [ ] Confirm all 5 build-and-sign jobs pass (Linux x86_64, Linux ARM64, Windows x86_64, macOS ARM64, VSCode extension).
- [ ] Confirm all 5 SLSA provenance jobs pass.
- [ ] Approve environment gate `oss-release` on `verivus-oss/sqry` to allow publish job
- [ ] Confirm `Publish OCI Image` job passes (GHCR push, multi-arch manifest, Cosign verify).
- [ ] Confirm publish job passes (checksum verification -> Cosign verification -> GitHub Release creation).
- [ ] Confirm release notes describe one recommended install path per OS and label `.bundle` files as verification artifacts.

## Coverage Verification (Language Plugins)
- [ ] Run `cargo tarpaulin -p sqry-lang-rust --out Html --output-dir target/tarpaulin-rust`
- [ ] Run `cargo tarpaulin -p sqry-lang-javascript --out Html --output-dir target/tarpaulin-javascript`
- [ ] Run `cargo tarpaulin -p sqry-lang-typescript --out Html --output-dir target/tarpaulin-typescript`
- [ ] Run `cargo tarpaulin -p sqry-lang-python --out Html --output-dir target/tarpaulin-python`
- [ ] Run `cargo tarpaulin -p sqry-lang-go --out Html --output-dir target/tarpaulin-go`
- [ ] Run `cargo tarpaulin -p sqry-lang-java --out Html --output-dir target/tarpaulin-java`
- [ ] Confirm relation modules meet ≥85% coverage for each language.
- [ ] Record coverage results in `docs/development/<component>/06_TEST_EXECUTION.md`.
- [ ] If any module falls below 85%, open a follow-up issue and document the link in the release notes.

## Relation Regression Spot Checks
- [ ] Run `sqry relations callers` smoke tests on Tier-1 language fixtures.
- [ ] Run `sqry relations imports` / `exports` on representative fixtures and verify outputs match expectations.

## Documentation
- [ ] Update `CHANGELOG.md` with coverage and relation highlights.
- [ ] Ensure `docs/guides/RELATIONS.md` reflects the latest language capabilities.

## Supply Chain Transparency

### SBOM & VEX
- [ ] Confirm the `Generate SBOM + VEX` workflow succeeded for the release tag (`gh run list --workflow "Generate SBOM + VEX"`).
- [ ] Download the workflow artifact (`gh run download <run-id> --name sbom-vex`) and verify SHA-256 sums for `sbom-cyclonedx.json`, `sbom-spdx.json`, `sbom-openvex.json`, `vulnerabilities.json`, and `vulnerabilities.txt`.
- [ ] Verify the GitHub release contains the same five assets with matching hashes.
- [ ] Attach the checksum file and verification notes to `06_TEST_EXECUTION.md`.

### Signing & Provenance (SLSA Level 2 Per-Platform)
- [ ] Confirm the `OSS Leg 3 Release` workflow (`oss-leg3-release.yml`) succeeded on `verivus-oss/sqry` for the release tag: `gh run list --repo verivus-oss/sqry --workflow "OSS Leg 3 Release"`
- [ ] Verify all 5 build-and-sign jobs completed successfully (Linux x86_64, Linux ARM64, Windows x86_64, macOS ARM64, VSCode).
- [ ] Verify all 5 provenance jobs completed successfully (per-platform SLSA attestations).
- [ ] Download release artifacts from `https://github.com/verivus-oss/sqry/releases/tag/vX.Y.Z` and verify:
  - [ ] Windows primary package (`sqry-windows-x86_64.zip`) is present for normal users
  - [ ] 12 binaries (sqry, sqry-mcp, sqry-lsp × Linux x86_64, Linux ARM64, Windows x86_64, macOS ARM64)
  - [ ] 1 VSCode extension (`.vsix`)
  - [ ] 14 Cosign signature bundles (`.bundle` files, one per published binary/archive/extension)
  - [ ] Optional model archives on supported platforms (`sqry-models.tar.gz` + `.bundle` where present)
  - [ ] 5 SLSA provenance attestations (`*-provenance.intoto.jsonl`)
  - [ ] Per-platform checksums (`CHECKSUMS-linux.sha256`, `CHECKSUMS-windows.sha256`, `CHECKSUMS-macos-arm64.sha256`, `CHECKSUMS-vscode.sha256`)
  - [ ] Linux ARM64 checksum file (`CHECKSUMS-linux-arm64.sha256`)
  - [ ] Consolidated `CHECKSUMS.sha256`
- [ ] Confirm each non-checksum artifact is present in its platform checksum file (no missing checksum entries).
- [ ] Confirm package-manager assets/manifests point at the intended user-facing artifacts (for example, Windows ZIP instead of raw CLI exe where applicable).
- [ ] Confirm `sqry-windows-x86_64.zip.bundle` is present and covered by the Windows checksum/signing verification flow.
- [ ] Run `winget validate` against the generated Windows manifest and record the output.
- [ ] Record a Windows-host alias/install check confirming `sqry`, `sqry-mcp`, and `sqry-lsp` resolve after package installation.
- [ ] Verify checksums: `sha256sum -c CHECKSUMS.sha256` (or `shasum -a 256 -c CHECKSUMS.sha256` on macOS).
- [ ] Spot-check Cosign signatures (at least 1 binary per platform):
  ```bash
  cosign verify-blob \
    --bundle sqry-linux-x86_64.bundle \
    --certificate-identity-regexp \
      "^https://github\.com/verivus-oss/sqry/\.github/workflows/oss-leg3-release\.yml@refs/tags/vX\.Y\.Z$" \
    --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
    sqry-linux-x86_64
  ```
- [ ] Spot-check SLSA provenance (at least 1 binary per platform):
  ```bash
  slsa-verifier verify-artifact sqry-linux-x86_64 \
    --provenance-path sqry-linux-x86_64-provenance.intoto.jsonl \
    --source-uri github.com/verivus-oss/sqry
  ```
- [ ] If `enable_signpath=true`, confirm SignPath Authenticode signing steps succeeded for Windows binaries, MSI, and VSIX and record evidence in `06_TEST_EXECUTION.md`.
- [ ] Confirm Cosign cert identity encodes `verivus-oss/sqry`.
- [ ] Confirm all provenance files reference the correct source commit and `verivus-oss/sqry`.
- [ ] Confirm OCI image `ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z` exists in GHCR and resolves to a multi-arch manifest (`linux/amd64`, `linux/arm64`).
- [ ] Verify OCI image signature:
  ```bash
  cosign verify ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z \
    --certificate-identity-regexp \
      "^https://github\.com/verivus-oss/sqry/\.github/workflows/oss-leg3-release\.yml@refs/tags/vX\.Y\.Z$" \
    --certificate-oidc-issuer "https://token.actions.githubusercontent.com"
  ```
- [ ] Confirm `.mcp/server.json` in the public repo points at `ghcr.io/verivus-oss/sqry-mcp:vX.Y.Z` with the expected bind-mount and `SQRY_MCP_WORKSPACE_ROOT=/workspace` runtime arguments.
- [ ] Record verification evidence (workflow run IDs, checksums, verification command outputs) in `06_TEST_EXECUTION.md`.

Check off each item before finalizing the release.
