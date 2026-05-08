#!/usr/bin/env bash
# Shared helpers for GitHub release asset uploads in release-control workflows.

gh_release_asset_uploaded() {
  local tag="$1"
  local asset_path="$2"
  shift 2

  local asset_name
  asset_name="$(basename "$asset_path")"

  gh release view "$tag" "$@" --json assets --jq '.assets[].name' \
    | grep -Fqx "$asset_name"
}

gh_release_upload_with_retry() {
  local tag="$1"
  local asset_path="$2"
  shift 2

  local attempts="${GH_RELEASE_UPLOAD_ATTEMPTS:-5}"
  local delay_seconds="${GH_RELEASE_UPLOAD_RETRY_DELAY_SECONDS:-10}"
  local attempt

  for attempt in $(seq 1 "$attempts"); do
    if gh release upload "$tag" "$@" "$asset_path" --clobber \
      && gh_release_asset_uploaded "$tag" "$asset_path" "$@"; then
      return 0
    fi

    if [[ "$attempt" -lt "$attempts" ]]; then
      echo "release asset upload for $(basename "$asset_path") failed verification; retrying in ${delay_seconds}s (${attempt}/${attempts})" >&2
      sleep "$delay_seconds"
    fi
  done

  echo "FATAL: release asset upload for $(basename "$asset_path") failed after ${attempts} attempts" >&2
  return 1
}
