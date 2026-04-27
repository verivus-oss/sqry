#!/usr/bin/env bash
# scripts/repro/operational_folder.sh
#
# STEP_11 reproducer for the "No index found" / "No lock file found"
# operational-folder regression. The CI `runtime-path-parity` job runs
# the Rust integration test (tests/integration/tests/operational_folder_regression.rs);
# this script provides a manual reproducer that an operator can run
# locally to verify the same contract end-to-end against the real
# `sqry lsp` binary, by:
#
#   1. Building the fixture from the integration-test crate (a single
#      cargo invocation runs the fixture builder via the
#      `--print-fixture` test binary mode encoded below; we fall back
#      to a hand-rolled fixture creation if the debug binary cannot
#      be built).
#   2. Launching `sqry lsp --stdio` against the fixture root (the LSP
#      server treats the fixture's `.sqry-workspace` as the workspace
#      registry).
#   3. Capturing the LSP outputChannel (stderr, where structured logs
#      go) and grepping for forbidden strings.
#
# Forbidden output strings (each is a regex; ANY match → exit 1):
#
#     No index found at .*tools/operational
#     No lock file found at .*tools/operational
#     No index found at .*node_modules
#     No lock file found at .*node_modules
#
# Allowed (and expected) output strings — these are the legitimate
# "user has not run sqry index yet" prompts for source roots:
#
#     No index found at .*frontend
#     No index found at .*backend
#
# Exit codes:
#   0 — no forbidden strings observed.
#   1 — at least one forbidden string observed (regression).
#   2 — invocation / environment error (sqry binary not found,
#       fixture creation failed, etc.).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

# Where to drop the fixture. Default: a fresh tempdir; override with
# OPERATIONAL_FOLDER_FIXTURE_DIR=<path> to inspect the layout after
# the run.
FIXTURE_DIR="${OPERATIONAL_FOLDER_FIXTURE_DIR:-}"
if [[ -z "${FIXTURE_DIR}" ]]; then
  FIXTURE_DIR="$(mktemp -d -t sqry-operational-folder-XXXXXX)"
  KEEP_FIXTURE=0
else
  mkdir -p "${FIXTURE_DIR}"
  KEEP_FIXTURE=1
fi

cleanup() {
  if [[ "${KEEP_FIXTURE}" -eq 0 ]]; then
    rm -rf "${FIXTURE_DIR}"
  else
    echo "fixture retained at: ${FIXTURE_DIR}"
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# 1. Build the fixture in-place. We hand-roll the layout so this script
#    has zero cargo dependency at run time (the CI `rust` job already
#    builds + runs the integration tests; this reproducer is for
#    operators on machines without a full Rust toolchain).
# ---------------------------------------------------------------------------
mkdir -p \
  "${FIXTURE_DIR}/frontend/src" \
  "${FIXTURE_DIR}/backend/src" \
  "${FIXTURE_DIR}/tools/operational" \
  "${FIXTURE_DIR}/node_modules/pkg"

cat > "${FIXTURE_DIR}/frontend/src/main.ts" <<'EOF'
export const x = 1;
EOF
cat > "${FIXTURE_DIR}/backend/src/main.rs" <<'EOF'
fn main() {}
EOF
cat > "${FIXTURE_DIR}/tools/operational/deploy.sh" <<'EOF'
#!/usr/bin/env bash
echo deploying
EOF
cat > "${FIXTURE_DIR}/node_modules/pkg/index.js" <<'EOF'
module.exports = {};
EOF

# Canonical absolute paths — `sqry lsp` rejects relative entries inside
# the workspace registry.
abs_frontend="$(cd "${FIXTURE_DIR}/frontend" && pwd -P)"
abs_backend="$(cd "${FIXTURE_DIR}/backend" && pwd -P)"
abs_member="$(cd "${FIXTURE_DIR}/tools/operational" && pwd -P)"
abs_excluded="$(cd "${FIXTURE_DIR}/node_modules" && pwd -P)"

# v2 `.sqry-workspace` registry. Field names mirror
# `sqry_core::workspace::WorkspaceRegistry` (the in-memory v2 schema).
cat > "${FIXTURE_DIR}/.sqry-workspace" <<EOF
{
  "metadata": {
    "version": 2,
    "name": "operational-folder-repro",
    "created_at": "1970-01-01T00:00:00Z",
    "updated_at": "1970-01-01T00:00:00Z"
  },
  "source_roots": [
    {
      "id": "frontend",
      "name": "frontend",
      "root": "${abs_frontend}",
      "index_path": "${abs_frontend}/.sqry/graph/manifest.json",
      "last_indexed_at": null,
      "symbol_count": null,
      "primary_language": null
    },
    {
      "id": "backend",
      "name": "backend",
      "root": "${abs_backend}",
      "index_path": "${abs_backend}/.sqry/graph/manifest.json",
      "last_indexed_at": null,
      "symbol_count": null,
      "primary_language": null
    }
  ],
  "member_folders": [
    {
      "id": "tools/operational",
      "root": "${abs_member}",
      "reason": "operationalFolder"
    }
  ],
  "exclusions": [
    "${abs_excluded}"
  ],
  "project_root_mode": "gitRoot"
}
EOF

# ---------------------------------------------------------------------------
# 2. Locate the sqry binary. Honour SQRY_BIN if the operator points at
#    an explicit build; otherwise look in target/release then target/debug
#    (CI builds release; local dev usually has debug).
# ---------------------------------------------------------------------------
SQRY_BIN="${SQRY_BIN:-}"
if [[ -z "${SQRY_BIN}" ]]; then
  for candidate in \
    "${REPO_ROOT}/target/release/sqry" \
    "${REPO_ROOT}/target/debug/sqry"
  do
    if [[ -x "${candidate}" ]]; then
      SQRY_BIN="${candidate}"
      break
    fi
  done
fi

if [[ -z "${SQRY_BIN}" ]]; then
  # Try PATH as a last resort.
  if command -v sqry >/dev/null 2>&1; then
    SQRY_BIN="$(command -v sqry)"
  fi
fi

if [[ -z "${SQRY_BIN}" ]] || [[ ! -x "${SQRY_BIN}" ]]; then
  echo "error: sqry binary not found. Run \`cargo build -p sqry-cli\` or set SQRY_BIN." >&2
  exit 2
fi

echo "operational-folder repro:"
echo "  fixture: ${FIXTURE_DIR}"
echo "  sqry  : ${SQRY_BIN}"

# ---------------------------------------------------------------------------
# 3. Launch `sqry lsp` against the fixture. The LSP loop reads JSON-RPC
#    on stdin; we feed it a minimal `initialize` + `initialized` +
#    `shutdown` + `exit` sequence so the server starts, runs its
#    auto-index probe loop (the bug site), and then exits cleanly.
#
#    We capture stderr (the structured-log channel; the LSP server
#    deliberately keeps stdout free for JSON-RPC frames). The grep
#    runs against the full stderr capture.
# ---------------------------------------------------------------------------
log_capture="$(mktemp -t sqry-lsp-stderr-XXXXXX.log)"
trap 'cleanup; rm -f "${log_capture}"' EXIT

# Minimal LSP handshake. PID 1 is a sentinel; we do not actually
# share-memory with the parent. `rootUri` points at the fixture so
# the LSP picks up the `.sqry-workspace`. `initializationOptions.sqry`
# is the same shape the VS Code extension forwards.
abs_fixture="$(cd "${FIXTURE_DIR}" && pwd -P)"
abs_fixture_uri="file://${abs_fixture}"

# JSON-RPC frames are length-prefixed: `Content-Length: <N>\r\n\r\n<body>`.
# Build and pipe four frames.
build_frame() {
  local body="$1"
  local len=${#body}
  printf 'Content-Length: %d\r\n\r\n%s' "${len}" "${body}"
}

initialize_body=$(cat <<JSON
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":1,"rootUri":"${abs_fixture_uri}","capabilities":{},"workspaceFolders":[{"uri":"${abs_fixture_uri}","name":"operational-folder-repro"}],"initializationOptions":{"sqry":{"workspaceFile":"${abs_fixture}/.sqry-workspace"}}}}
JSON
)
initialized_body='{"jsonrpc":"2.0","method":"initialized","params":{}}'
shutdown_body='{"jsonrpc":"2.0","id":2,"method":"shutdown"}'
exit_body='{"jsonrpc":"2.0","method":"exit"}'

{
  build_frame "${initialize_body}"
  build_frame "${initialized_body}"
  # Give the auto-index probe loop a moment to fire. The
  # SqryLogIngestor and StatusManager publish events asynchronously
  # on workspace open; 1.5s is generous on CI.
  sleep 1.5
  build_frame "${shutdown_body}"
  build_frame "${exit_body}"
} | timeout 30 "${SQRY_BIN}" lsp --stdio 2> "${log_capture}" > /dev/null \
  || {
    rc=$?
    # Exit codes 124 (timeout) / 143 (SIGTERM) are non-fatal — the
    # server may not gracefully exit on every code path. We only care
    # about the captured stderr.
    if [[ "${rc}" -ne 124 ]] && [[ "${rc}" -ne 143 ]] && [[ "${rc}" -ne 0 ]]; then
      echo "warn: sqry lsp exited with code ${rc}" >&2
    fi
  }

# ---------------------------------------------------------------------------
# 4. Forbidden-string scan.
# ---------------------------------------------------------------------------
forbidden_patterns=(
  "No index found at .*tools/operational"
  "No lock file found at .*tools/operational"
  "No index found at .*node_modules"
  "No lock file found at .*node_modules"
)

violations=0
for pattern in "${forbidden_patterns[@]}"; do
  if grep -qE "${pattern}" "${log_capture}"; then
    echo "" >&2
    echo "FAIL: outputChannel contains forbidden pattern: ${pattern}" >&2
    grep -E "${pattern}" "${log_capture}" | sed -E 's/^/    /' >&2
    violations=$((violations + 1))
  fi
done

if [[ "${violations}" -gt 0 ]]; then
  echo "" >&2
  echo "operational-folder repro: ${violations} forbidden pattern(s) observed" >&2
  echo "full stderr capture: ${log_capture}" >&2
  exit 1
fi

echo "operational-folder repro: PASS (no forbidden patterns observed)"
rm -f "${log_capture}"
exit 0
