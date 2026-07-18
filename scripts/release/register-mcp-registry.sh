#!/usr/bin/env bash
set -euo pipefail
#
# Register/update sqry on the official MCP registry
# (https://registry.modelcontextprotocol.io), namespace io.github.verivus-oss/sqry.
#
# Two authentication modes:
#
#   OIDC (CI, default inside GitHub Actions):
#     MCP_REGISTRY_AUTH=oidc ./scripts/release/register-mcp-registry.sh
#   Runs inside a workflow in the verivus-oss org, so GitHub OIDC authorizes the
#   io.github.verivus-oss namespace with no stored secret. This is how the
#   release pipeline registers each version (.github/workflows-public/
#   publish-container.yml, on release: published).
#
#   Token exchange (manual/operator):
#     GITHUB_TOKEN=<token> ./scripts/release/register-mcp-registry.sh
#     ./scripts/release/register-mcp-registry.sh <github-token>
#   The token must belong to an identity that owns the verivus-oss namespace.
#
# The published manifest is sqry-mcp/server.json (kept byte-identical to
# .mcp/server.json by scripts/sync-versions.sh). The version is (re)patched from
# the workspace Cargo.toml so the registry entry always tracks the release.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

REGISTRY_API="https://registry.modelcontextprotocol.io/v0.1"
MANIFEST="${REPO_ROOT}/sqry-mcp/server.json"
GITHUB_TOKEN="${GITHUB_TOKEN:-${1:-}}"

# Auth mode: explicit MCP_REGISTRY_AUTH wins; otherwise github-at when a token is
# present, else oidc (the CI default when the workflow grants id-token: write).
AUTH_MODE="${MCP_REGISTRY_AUTH:-}"
if [[ -z "$AUTH_MODE" ]]; then
  if [[ -n "$GITHUB_TOKEN" ]]; then AUTH_MODE="github-at"; else AUTH_MODE="oidc"; fi
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo "FATAL: Server manifest not found: $MANIFEST" >&2
  exit 1
fi

# Patch version from workspace Cargo.toml so the registry entry tracks the release.
WORKSPACE_VERSION=$(grep -A5 '\[workspace.package\]' "$REPO_ROOT/Cargo.toml" \
  | grep '^version' | head -1 \
  | sed 's/.*= *"\([^"]*\)".*/\1/')
echo "[INFO] Workspace version: $WORKSPACE_VERSION"
echo "[INFO] Auth mode: $AUTH_MODE"

MANIFEST_TMP=$(mktemp)
trap 'rm -f "$MANIFEST_TMP"' EXIT
# Patch the top-level version, each package version, AND the oci identifier's
# embedded tag (ghcr.io/...:vX.Y.Z). Patching the identifier tag too means a
# stale checked-in manifest can never publish a version/identifier mismatch,
# even when this script is run standalone without scripts/sync-versions.sh.
jq --arg ver "$WORKSPACE_VERSION" '
  .version = $ver
  | if .packages then .packages |= map(
      if .registryType == "oci"
      then .version = ("v" + $ver) | .identifier = (.identifier | sub(":[^:]*$"; ":v" + $ver))
      else .version = $ver
      end
    ) else . end
' "$MANIFEST" > "$MANIFEST_TMP"

# Best-effort propagation check. The publish step already succeeded before this
# runs, and registry propagation can lag by minutes, so poll with backoff and
# NEVER fail the release on a verification-only timeout (that would red a
# release whose publish actually worked).
verify_registration() {
  echo ""
  echo "[INFO] Verifying registration (publish succeeded; polling for propagation)..."
  local attempts=6 delay=15 resp code published i
  for (( i=1; i<=attempts; i++ )); do
    sleep "$delay"
    resp="$(mktemp)"
    code=$(curl -s -o "$resp" -w "%{http_code}" \
      "${REGISTRY_API}/servers/io.github.verivus-oss%2Fsqry/versions/latest")
    if [[ "$code" == "200" ]]; then
      published="$(jq -r '.server.version // empty' "$resp")"
      rm -f "$resp"
      if [[ "$published" == "$WORKSPACE_VERSION" ]]; then
        echo "[OK] Registry version matches workspace: ${published}"
        return 0
      fi
      echo "[INFO] Attempt ${i}/${attempts}: registry shows '${published:-<missing>}', want '${WORKSPACE_VERSION}'; retrying in ${delay}s"
    else
      rm -f "$resp"
      echo "[INFO] Attempt ${i}/${attempts}: verify HTTP ${code}; retrying in ${delay}s"
    fi
  done
  echo "[WARN] Registry did not show ${WORKSPACE_VERSION} after $((attempts * delay))s; publish succeeded, propagation may still be pending." >&2
  return 0
}

if [[ "$AUTH_MODE" == "oidc" ]]; then
  if ! command -v mcp-publisher >/dev/null 2>&1; then
    echo "[INFO] Installing mcp-publisher CLI..."
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')
    tmpbin=$(mktemp -d)
    curl -fsSL "https://github.com/modelcontextprotocol/registry/releases/latest/download/mcp-publisher_${os}_${arch}.tar.gz" \
      | tar -xz -C "$tmpbin" mcp-publisher
    export PATH="$tmpbin:$PATH"
  fi

  # mcp-publisher publishes ./server.json from its working directory.
  workdir=$(mktemp -d)
  cp "$MANIFEST_TMP" "$workdir/server.json"
  echo "[INFO] Authenticating via GitHub OIDC..."
  ( cd "$workdir" && mcp-publisher login github-oidc )
  echo "[INFO] Publishing to MCP registry..."
  set +e
  publish_out=$( cd "$workdir" && mcp-publisher publish 2>&1 )
  publish_rc=$?
  set -e
  echo "$publish_out"
  rm -rf "$workdir"
  if [[ $publish_rc -ne 0 ]]; then
    if echo "$publish_out" | grep -qi "duplicate version"; then
      echo "[INFO] Registry already has version ${WORKSPACE_VERSION}; verifying latest endpoint"
    else
      echo "FATAL: Publish failed" >&2
      exit 1
    fi
  else
    echo "[OK] Published successfully"
  fi
  verify_registration
  exit 0
fi

# --- github-at token-exchange mode (manual/operator) ---
if [[ -z "$GITHUB_TOKEN" ]]; then
  echo "FATAL: github-at mode needs a token. Set GITHUB_TOKEN or pass it as the first argument (or use MCP_REGISTRY_AUTH=oidc in CI)." >&2
  exit 1
fi

echo "[INFO] Exchanging GitHub token for registry JWT..."
JWT_RESPONSE=$(curl -sf -X POST "${REGISTRY_API}/auth/github-at" \
  -H "Content-Type: application/json" \
  -d "{\"github_token\": \"${GITHUB_TOKEN}\"}")

REGISTRY_TOKEN=$(echo "$JWT_RESPONSE" | jq -r '.registry_token // .token // empty')
if [[ -z "$REGISTRY_TOKEN" ]]; then
  echo "FATAL: Failed to exchange GitHub token for registry JWT" >&2
  echo "$JWT_RESPONSE" >&2
  exit 1
fi
echo "[OK] Registry JWT obtained"

echo "[INFO] Validating manifest..."
VALIDATE_RESPONSE=$(curl -s -X POST "${REGISTRY_API}/validate" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer ${REGISTRY_TOKEN}" \
  -d @"$MANIFEST_TMP")
if echo "$VALIDATE_RESPONSE" | jq -e '.status >= 400' >/dev/null 2>&1; then
  echo "FATAL: Manifest validation failed" >&2
  echo "$VALIDATE_RESPONSE" | jq . >&2
  exit 1
fi
echo "[OK] Manifest valid"

echo "[INFO] Publishing to MCP registry..."
PUBLISH_RESPONSE=$(curl -s -X POST "${REGISTRY_API}/publish" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer ${REGISTRY_TOKEN}" \
  -d @"$MANIFEST_TMP")

if echo "$PUBLISH_RESPONSE" | jq -e '.status >= 400' >/dev/null 2>&1; then
  if echo "$PUBLISH_RESPONSE" | jq -e --arg ver "$WORKSPACE_VERSION" '
      (.errors // []) | any(.message == "invalid version: cannot publish duplicate version")
    ' >/dev/null 2>&1; then
    echo "[INFO] Registry already has version ${WORKSPACE_VERSION}; verifying latest endpoint"
  else
    echo "FATAL: Publish failed" >&2
    echo "$PUBLISH_RESPONSE" | jq . >&2
    exit 1
  fi
else
  echo "[OK] Published successfully"
  echo "$PUBLISH_RESPONSE" | jq .
fi

verify_registration
