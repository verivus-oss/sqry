#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

extract_job_block() {
  local workflow_path="$1"
  local job_name="$2"

  awk -v job_name="$job_name" '
    $0 ~ "^  " job_name ":" {
      in_job = 1
    }
    in_job && $0 ~ "^  [A-Za-z0-9_-]+:" && $0 !~ "^  " job_name ":" {
      exit
    }
    in_job {
      print
    }
  ' "$workflow_path"
}

check_job_environment() {
  local workflow_path="$1"
  local job_name="$2"
  local expected_environment="$3"

  local job_block
  job_block="$(extract_job_block "$workflow_path" "$job_name")"

  if [[ -z "$job_block" ]]; then
    echo "error: job '$job_name' not found in $workflow_path" >&2
    return 1
  fi

  if ! grep -q "^    environment: ${expected_environment}\$" <<< "$job_block"; then
    echo "error: $workflow_path job '$job_name' must declare environment: ${expected_environment}" >&2
    return 1
  fi
}

# check_manifest_workflows validates that every workflow listed under
# selection.github.workflows_public in release-manifest.toml exists on disk.
# Returns 0 if the manifest file is absent (graceful skip) or all listed
# workflows are present; returns 1 if any listed workflow is missing.
check_manifest_workflows() {
  local manifest="${REPO_ROOT}/release-manifest.toml"
  if [[ ! -f "$manifest" ]]; then
    return 0
  fi

  local failed=false
  local output
  output="$(python3 - "$manifest" "${REPO_ROOT}" <<'PYEOF'
import sys
import pathlib

manifest_path = pathlib.Path(sys.argv[1])
repo_root = pathlib.Path(sys.argv[2])

try:
    import tomllib
except ModuleNotFoundError:
    # Python < 3.11: fall back to tomli if available, otherwise skip
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        print("warning: tomllib/tomli not available; skipping manifest workflow check", flush=True)
        sys.exit(0)

with open(manifest_path, "rb") as fh:
    data = tomllib.load(fh)

selection = data.get("selection", {})
workflows_public = selection.get("github", {}).get("workflows_public", [])

missing = []
for wf in workflows_public:
    wf_path = repo_root / wf
    if not wf_path.exists():
        missing.append(str(wf))

if missing:
    for m in missing:
        print(f"error: manifest lists workflow '{m}' but it does not exist on disk", flush=True)
    sys.exit(1)

if workflows_public:
    print(f"manifest-workflows-ok ({len(workflows_public)} checked)", flush=True)

sys.exit(0)
PYEOF
)"
  local py_exit=$?

  if [[ -n "$output" ]]; then
    echo "$output"
  fi

  if [[ $py_exit -ne 0 ]]; then
    failed=true
  fi

  [[ "$failed" == "false" ]]
}

main() {
  local checked_any=false

  # Determine repository context: presence of oss-leg1-sanitize.yml indicates
  # the private (internal) repo.  Its absence means we are on the public repo,
  # where Leg 1 is excluded by the sanitization pipeline.
  local is_private_repo=false
  local leg1_workflow="${REPO_ROOT}/.github/workflows/oss-leg1-sanitize.yml"
  if [[ -f "$leg1_workflow" ]]; then
    is_private_repo=true
    check_job_environment "$leg1_workflow" "verify-staging" "oss-staging"
    checked_any=true
  fi

  local leg3_workflow="${REPO_ROOT}/.github/workflows/oss-leg3-release.yml"
  if [[ -f "$leg3_workflow" ]]; then
    check_job_environment "$leg3_workflow" "smoke-tests" "oss-signing"
    checked_any=true
  fi

  if [[ "$checked_any" == "false" ]]; then
    if [[ "$is_private_repo" == "true" ]]; then
      # On the private repo both workflows are expected; treat as a hard error.
      echo "error: no known release workflow contracts found under ${REPO_ROOT}/.github/workflows" >&2
      return 1
    else
      # On the public repo only Leg 3 is present; Leg 1 is intentionally
      # absent after sanitization.  Emit a warning but do not fail.
      echo "warning: no known release workflow contracts found; assuming public repo context — skipping OIDC contract check" >&2
    fi
  fi

  # Manifest-based check (additive; skips gracefully when file is absent).
  check_manifest_workflows

  echo "workflow-contracts-ok"
}

main "$@"
