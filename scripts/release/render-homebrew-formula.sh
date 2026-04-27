#!/usr/bin/env bash
#
# render-homebrew-formula.sh — Render packaging/homebrew/sqry.rb for a release tag.
#
# Downloads SHA256SUMS.txt from the named GitHub release, parses out the 16
# brew-relevant binary checksums, substitutes every @PLACEHOLDER@ in the
# template, and writes a publish-ready Formula/sqry.rb.
#
# Usage:
#   scripts/release/render-homebrew-formula.sh \
#     --tag v9.0.23 \
#     [--template packaging/homebrew/sqry.rb] \
#     [--out Formula/sqry.rb] \
#     [--repo verivus-oss/sqry]
#
# Required tooling: bash, curl OR gh, python3 (string substitution + audit).
# Optional tooling: brew (best-effort `brew style` lint), ruby (`ruby -c`).
#
# Exit codes:
#   0 — rendered successfully
#   1 — bad arguments / preflight failure
#   2 — SHA256SUMS missing or incomplete (see stderr for which assets are missing)
#   3 — substitution left @...@ tokens behind (template/script drift)
#
# Auth:
#   The GitHub release assets are publicly downloadable — no token required for
#   render. The companion publish workflow needs a separate PAT secret named
#   HOMEBREW_TAP_PUSH_TOKEN with `contents:write` on verivus-oss/homebrew-sqry.

set -euo pipefail

TEMPLATE="packaging/homebrew/sqry.rb"
OUT="Formula/sqry.rb"
REPO="verivus-oss/sqry"
TAG=""

usage() {
    cat >&2 <<'USAGE'
Usage: render-homebrew-formula.sh --tag <vX.Y.Z> [options]

Options:
  --tag <vX.Y.Z>      Release tag to render (required, must include 'v' prefix)
  --template <path>   Template file (default: packaging/homebrew/sqry.rb)
  --out <path>        Output formula path (default: Formula/sqry.rb)
  --repo <owner/name> Source repo (default: verivus-oss/sqry)
  -h, --help          Show this help
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tag)      TAG="$2"; shift 2 ;;
        --template) TEMPLATE="$2"; shift 2 ;;
        --out)      OUT="$2"; shift 2 ;;
        --repo)     REPO="$2"; shift 2 ;;
        -h|--help)  usage; exit 0 ;;
        *)          echo "ERROR: unknown argument: $1" >&2; usage; exit 1 ;;
    esac
done

if [[ -z "$TAG" ]]; then
    echo "ERROR: --tag is required" >&2
    usage
    exit 1
fi

# Validate tag format: vMAJOR.MINOR.PATCH[-prerelease].
if [[ ! "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.]+)?$ ]]; then
    echo "ERROR: invalid tag format: '$TAG' (expected vX.Y.Z[-prerelease])" >&2
    exit 1
fi

if [[ ! -f "$TEMPLATE" ]]; then
    echo "ERROR: template not found: $TEMPLATE" >&2
    exit 1
fi

VERSION="${TAG#v}"

echo "render-homebrew-formula: tag=$TAG version=$VERSION repo=$REPO" >&2
echo "render-homebrew-formula: template=$TEMPLATE out=$OUT" >&2

# Prepare working directory.
mkdir -p "$(dirname "$OUT")"

# Download SHA256SUMS.txt. Prefer gh (auth + rate-limit-friendly) when available.
SHA_FILE="$(mktemp)"
trap 'rm -f "$SHA_FILE"' EXIT

if command -v gh >/dev/null 2>&1; then
    echo "render-homebrew-formula: downloading SHA256SUMS.txt via gh" >&2
    gh release download "$TAG" \
        --repo "$REPO" \
        --pattern SHA256SUMS.txt \
        --output "$SHA_FILE" \
        --clobber
else
    echo "render-homebrew-formula: downloading SHA256SUMS.txt via curl" >&2
    curl -fsSL \
        "https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS.txt" \
        -o "$SHA_FILE"
fi

if [[ ! -s "$SHA_FILE" ]]; then
    echo "ERROR: SHA256SUMS.txt is empty for $TAG" >&2
    exit 2
fi

# Parse a single sha for a named asset. Fail loudly if missing.
get_sha() {
    local asset="$1"
    local sha
    sha="$(awk -v a="$asset" '$2 == a { print $1; exit }' "$SHA_FILE")"
    if [[ -z "$sha" ]]; then
        echo "ERROR: missing SHA256 for asset '$asset' in $TAG SHA256SUMS.txt" >&2
        exit 2
    fi
    printf '%s' "$sha"
}

# 16 brew-relevant binaries: 4 binaries × 4 platforms.
declare -A SHA
for binary in sqry sqry-mcp sqry-lsp sqryd; do
    for platform in macos-arm64 macos-x86_64 linux-arm64 linux-x86_64; do
        asset="${binary}-${platform}"
        sha="$(get_sha "$asset")"
        # Build placeholder name: BINARY_PLATFORM uppercased, hyphens → underscores.
        upper_bin="$(printf '%s' "$binary" | tr '[:lower:]-' '[:upper:]_')"
        upper_plat="$(printf '%s' "$platform" | tr '[:lower:]-' '[:upper:]_')"
        key="SHA_${upper_bin}_${upper_plat}"
        SHA["$key"]="$sha"
        echo "  found  $asset -> @${key}@ = ${sha:0:12}…" >&2
    done
done

# Substitute placeholders via python3 (safe for arbitrary chars; deterministic).
python3 - "$TEMPLATE" "$OUT" "$VERSION" "$TAG" "$REPO" <<'PY' "${!SHA[@]}" "${SHA[@]}"
import os, sys, re, pathlib

template_path, out_path, version, tag, repo = sys.argv[1:6]
rest = sys.argv[6:]
half = len(rest) // 2
keys = rest[:half]
values = rest[half:]
shas = dict(zip(keys, values))

text = pathlib.Path(template_path).read_text()
substitutions = {
    "@VERSION@": version,
    "@VERSION_TAG@": tag,
    "@REPO@": repo,
}
substitutions.update({f"@{k}@": v for k, v in shas.items()})

for placeholder, value in substitutions.items():
    if placeholder in text:
        text = text.replace(placeholder, value)
        print(f"  subst {placeholder} -> {value[:48]}", file=sys.stderr)
    else:
        print(f"  WARN  {placeholder} not present in template", file=sys.stderr)

# Audit: no remaining @...@ tokens.
remaining = re.findall(r"@[A-Z0-9_]+@", text)
if remaining:
    print(
        f"ERROR: substitution incomplete; remaining tokens: {sorted(set(remaining))}",
        file=sys.stderr,
    )
    sys.exit(3)

pathlib.Path(out_path).parent.mkdir(parents=True, exist_ok=True)
pathlib.Path(out_path).write_text(text)
print(f"render-homebrew-formula: wrote {out_path} ({len(text)} bytes)", file=sys.stderr)
PY

# Optional: brew style (best-effort).
if command -v brew >/dev/null 2>&1; then
    echo "render-homebrew-formula: running 'brew style' (best-effort)" >&2
    brew style "$OUT" >&2 || echo "render-homebrew-formula: brew style emitted warnings (non-fatal)" >&2
fi

echo "render-homebrew-formula: done — $OUT" >&2
