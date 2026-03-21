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

main() {
  local checked_any=false

  local leg1_workflow="${REPO_ROOT}/.github/workflows/oss-leg1-sanitize.yml"
  if [[ -f "$leg1_workflow" ]]; then
    check_job_environment "$leg1_workflow" "verify-staging" "oss-staging"
    checked_any=true
  fi

  local leg3_workflow="${REPO_ROOT}/.github/workflows/oss-leg3-release.yml"
  if [[ -f "$leg3_workflow" ]]; then
    check_job_environment "$leg3_workflow" "smoke-tests" "oss-signing"
    checked_any=true
  fi

  if [[ "$checked_any" == "false" ]]; then
    echo "error: no known release workflow contracts found under ${REPO_ROOT}/.github/workflows" >&2
    return 1
  fi

  echo "workflow-contracts-ok"
}

main "$@"
