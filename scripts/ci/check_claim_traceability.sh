#!/usr/bin/env bash
# scripts/ci/check_claim_traceability.sh
#
# STEP_11 claim-traceability gate: every workspace-aware / cross-repo /
# multi-root / logical-workspace claim made in user-facing documentation
# (README.md, docs/marketing/**, CHANGELOG.md, docs/FEATURE_LIST.md) must
# carry an inline `<!-- claim:<id> test:<test_name> -->` HTML comment, AND
# every such inline marker must:
#
#   1. Have a matching `[claims.<id>]` entry in
#      docs/development/workspace-aware-cross-repo/claims.toml.
#   2. The `test_name` recorded in that manifest entry must resolve
#      under a *per-claim* cargo dispatch:
#         cargo test -p <cargo_package> --test <cargo_test_target> \
#                    <test_name> -- --list
#      (when cargo_test_target == "lib", `--lib` is used instead of
#      `--test <target>`). TS tests prefixed `vscode:` are intentionally
#      not enforced here — the vscode-extension CI job runs the TS
#      suite separately.
#
#      Per-claim dispatch replaces the original
#      `cargo test --workspace -- --list` global resolver: that resolver
#      compiled the entire workspace just to list three test names, and
#      the resulting target/ tree exhausted the GitHub Actions runner's
#      ~14 GB of free disk on every CI run. Per-claim dispatch only
#      compiles the crate under test plus its transitive deps.
#
# Exit codes:
#   0 — every inline claim resolves; manifest is consistent with the docs.
#   1 — one or more violations (missing manifest entry, unresolvable
#       test name, manifest entry without sources). Detail to stderr.
#   2 — invocation / environment error.
#
# Why this gate exists (codex iter1 MAJOR):
#   The "configurable cross-repo classification" claim in README and
#   marketing copy was unverifiable for months — the cited test name
#   (`test_logical_workspace_resolution_branches`) didn't exist. STEP_9
#   discovered this during reconciliation; STEP_11 makes the recurrence
#   structurally impossible.
#
# Implementation notes:
#   - Pure bash + grep + awk + cargo. No toml parser; the claims.toml
#     schema is intentionally line-orientable so we can extract each
#     `[claims.<id>]` block with awk.
#   - Inline marker format is exactly:
#         <!-- claim:<id> test:<test_name> -->
#     where <id> matches `[a-zA-Z0-9_-]+` and <test_name> matches
#     `[a-zA-Z0-9_:.-]+`. (The `:` allows the `vscode:` prefix.)
#   - The cargo test list is computed once and cached in $RUNNER_TEMP
#     (or /tmp) for the duration of a CI job to avoid re-running it
#     when this script is invoked alongside other gates.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PRIVATE_MANIFEST="${REPO_ROOT}/docs/development/workspace-aware-cross-repo/claims.toml"
PUBLIC_MANIFEST="${REPO_ROOT}/docs/claim-traceability.toml"

DOC_GLOBS=(
  "README.md"
  "CHANGELOG.md"
  "docs/FEATURE_LIST.md"
  "docs/marketing"
)

if [[ -f "${PRIVATE_MANIFEST}" ]]; then
  MANIFEST="${PRIVATE_MANIFEST}"
elif [[ -f "${PUBLIC_MANIFEST}" ]]; then
  MANIFEST="${PUBLIC_MANIFEST}"
else
  echo "error: manifest not found: ${PRIVATE_MANIFEST} or ${PUBLIC_MANIFEST}" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# 1. Collect inline claims from documentation surfaces.
#    Format produced (one per line):  <id>|<test_name>|<doc_path>:<line>
# ---------------------------------------------------------------------------
inline_claims="$(mktemp)"
manifest_meta="$(mktemp)"
trap 'rm -f "${inline_claims}" "${inline_ids}" "${manifest_ids}" "${id_only_in_inline}" "${id_only_in_manifest}" "${unresolved_tests}" "${manifest_pairs}" "${inline_pairs}" "${manifest_meta}"' EXIT

scan_doc() {
  local path="$1"
  # The marker must be on a single line. We capture id + test_name + lineno
  # and emit `<id>|<test>|<path>:<lineno>`. Use awk (not sed) to avoid
  # delimiter collisions with `|` in the output format.
  awk -v p="${path}" '
    match($0, /<!--[[:space:]]+claim:[a-zA-Z0-9_-]+[[:space:]]+test:[a-zA-Z0-9_:.\-]+[[:space:]]+-->/) {
      blob = substr($0, RSTART, RLENGTH)
      # Strip leading "<!-- " and trailing " -->".
      sub(/^<!--[[:space:]]+/, "", blob)
      sub(/[[:space:]]+-->$/, "", blob)
      # blob is now: claim:<id> test:<name>
      n = split(blob, parts, /[[:space:]]+/)
      id = ""
      tn = ""
      for (i = 1; i <= n; i++) {
        if (parts[i] ~ /^claim:/) {
          id = parts[i]; sub(/^claim:/, "", id)
        } else if (parts[i] ~ /^test:/) {
          tn = parts[i]; sub(/^test:/, "", tn)
        }
      }
      if (id != "" && tn != "") {
        printf "%s|%s|%s:%d\n", id, tn, p, NR
      }
    }
  ' "${path}" || true
}

for entry in "${DOC_GLOBS[@]}"; do
  full="${REPO_ROOT}/${entry}"
  if [[ -f "${full}" ]]; then
    scan_doc "${full}"
  elif [[ -d "${full}" ]]; then
    while IFS= read -r -d '' f; do
      scan_doc "${f}"
    done < <(find "${full}" -type f \( -name '*.md' -o -name '*.markdown' \) -print0)
  fi
done > "${inline_claims}"

# ---------------------------------------------------------------------------
# 2. Extract claim IDs + their declared test_names + (Rust) per-claim
#    cargo dispatch metadata from the manifest.
#
#    Two output files:
#      manifest_pairs : one line per claim, format <id>|<test_name>
#                      (used by id-set diff + inline-vs-manifest pair
#                      mismatch checks; both Rust and vscode: claims).
#      manifest_meta  : one line per *Rust* claim with full metadata,
#                      format <id>|<test_name>|<cargo_package>|<cargo_test_target>
#                      (used by per-claim cargo dispatch in section 5).
#                      vscode: claims are intentionally omitted.
#
#    awk state-machine parses each `[claims.<id>]` block; emits when
#    the block ends (next [section] header or EOF) so we can require
#    cargo_package + cargo_test_target on every Rust claim.
# ---------------------------------------------------------------------------
manifest_pairs="$(mktemp)"
awk -v meta_out="${manifest_meta}" '
  function flush() {
    if (cur_id == "") return
    if (cur_test == "") { cur_id = ""; return }
    print cur_id "|" cur_test
    if (cur_test ~ /^vscode:/) {
      # TS test — skip cargo metadata.
    } else {
      # Rust test — both fields are mandatory.
      if (cur_pkg == "" || cur_target == "") {
        printf "error: manifest claim %s (test_name=%s) is a Rust claim but missing cargo_package and/or cargo_test_target\n", cur_id, cur_test | "cat 1>&2"
        bad = 1
      } else {
        print cur_id "|" cur_test "|" cur_pkg "|" cur_target >> meta_out
      }
    }
    cur_id = ""; cur_test = ""; cur_pkg = ""; cur_target = ""
  }
  function strip_quoted(line) {
    val = line
    sub(/^[^"]*"/, "", val)
    sub(/".*$/, "", val)
    return val
  }
  /^\[claims\.[a-zA-Z0-9_-]+\][[:space:]]*$/ {
    flush()
    cur_id = $0
    sub(/^\[claims\./, "", cur_id)
    sub(/\][[:space:]]*$/, "", cur_id)
    cur_test = ""; cur_pkg = ""; cur_target = ""
    next
  }
  /^\[/ {
    # Non-claim section (e.g. [manifest]) — flush any in-progress block.
    flush()
    next
  }
  /^test_name[[:space:]]*=/ {
    if (cur_id != "" && cur_test == "") cur_test = strip_quoted($0)
    next
  }
  /^cargo_package[[:space:]]*=/ {
    if (cur_id != "" && cur_pkg == "") cur_pkg = strip_quoted($0)
    next
  }
  /^cargo_test_target[[:space:]]*=/ {
    if (cur_id != "" && cur_target == "") cur_target = strip_quoted($0)
    next
  }
  END { flush(); exit (bad ? 1 : 0) }
' "${MANIFEST}" \
  | sort -u > "${manifest_pairs}"
manifest_parse_status=${PIPESTATUS[0]}
if [[ "${manifest_parse_status}" -ne 0 ]]; then
  echo "FAIL: manifest validation failed (see errors above)" >&2
  exit 1
fi
sort -u -o "${manifest_meta}" "${manifest_meta}"

# ---------------------------------------------------------------------------
# 3. Identifier-set diffs.
# ---------------------------------------------------------------------------
inline_ids="$(mktemp)"
manifest_ids="$(mktemp)"

awk -F'|' '{ print $1 }' "${inline_claims}" | sort -u > "${inline_ids}"
awk -F'|' '{ print $1 }' "${manifest_pairs}" | sort -u > "${manifest_ids}"

id_only_in_inline="$(mktemp)"
id_only_in_manifest="$(mktemp)"
comm -23 "${inline_ids}" "${manifest_ids}" > "${id_only_in_inline}"
comm -13 "${inline_ids}" "${manifest_ids}" > "${id_only_in_manifest}"

inline_count="$(wc -l < "${inline_ids}")"
manifest_count="$(wc -l < "${manifest_ids}")"
echo "claim-traceability gate: ${inline_count} inline claim id(s), ${manifest_count} manifest entry/entries"

exit_code=0

if [[ -s "${id_only_in_inline}" ]]; then
  echo "" >&2
  echo "FAIL: inline claim id(s) without a matching [claims.<id>] manifest entry:" >&2
  while IFS= read -r mid; do
    [[ -z "${mid}" ]] && continue
    echo "  - ${mid}" >&2
    grep "^${mid}|" "${inline_claims}" | head -3 \
      | awk -F'|' '{ printf "      cited at: %s (test:%s)\n", $3, $2 }' >&2
  done < "${id_only_in_inline}"
  exit_code=1
fi

if [[ -s "${id_only_in_manifest}" ]]; then
  echo "" >&2
  echo "FAIL: manifest [claims.<id>] entries without any inline reference in docs:" >&2
  sed -E 's/^/  - /' "${id_only_in_manifest}" >&2
  exit_code=1
fi

# ---------------------------------------------------------------------------
# 4. Per-id consistency: the inline marker's test_name must match the
#    manifest entry's test_name, and the test_name must resolve under
#    `cargo test --workspace -- --list`. TS tests prefixed `vscode:` are
#    intentionally exempt (they are run by the vscode-extension CI job).
# ---------------------------------------------------------------------------
inline_pairs="$(mktemp)"
awk -F'|' '{ print $1 "|" $2 }' "${inline_claims}" | sort -u > "${inline_pairs}"

# Pair mismatch detection.
mismatched="$(mktemp)"
comm -23 "${inline_pairs}" "${manifest_pairs}" \
  | awk -F'|' '{ print $1 "|" $2 }' \
  | while IFS='|' read -r mid mtest; do
      manifest_test="$(awk -F'|' -v id="${mid}" '$1==id { print $2 }' "${manifest_pairs}")"
      if [[ -z "${manifest_test}" ]]; then
        # Already reported above as missing manifest entry. Skip.
        continue
      fi
      if [[ "${manifest_test}" != "${mtest}" ]]; then
        echo "  - ${mid}: inline test:${mtest} != manifest test_name=\"${manifest_test}\""
      fi
    done > "${mismatched}"

if [[ -s "${mismatched}" ]]; then
  echo "" >&2
  echo "FAIL: inline marker test_name does not match manifest test_name:" >&2
  cat "${mismatched}" >&2
  exit_code=1
fi
rm -f "${mismatched}"

# ---------------------------------------------------------------------------
# 5. Per-claim cargo dispatch: for every Rust claim in manifest_meta,
#    run `cargo test -p <pkg> [--lib | --test <target>] <test_name>
#    -- --list` and require the requested test name to appear in the
#    output.
#
#    Per-claim dispatch is the iter3 fix for the iter2 BLOCK: the prior
#    `cargo test --workspace -- --list` resolver compiled the entire
#    workspace just to enumerate three test names, exhausting runner
#    disk. Per-claim dispatch only compiles the crate under test plus
#    its transitive deps.
#
#    TS (`vscode:`) claims are exercised by the vscode-extension CI
#    job; they are filtered out at manifest_meta construction time and
#    skipped here.
# ---------------------------------------------------------------------------
unresolved_tests="$(mktemp)"
if [[ -s "${manifest_meta}" ]]; then
  while IFS='|' read -r mid mtest mpkg mtarget; do
    [[ -z "${mid}" ]] && continue
    # Build the cargo invocation. `lib` selects the library's unit
    # tests; anything else selects an integration test target.
    if [[ "${mtarget}" == "lib" ]]; then
      cargo_args=(test -p "${mpkg}" --lib --no-fail-fast "${mtest}" -- --list)
    else
      cargo_args=(test -p "${mpkg}" --test "${mtarget}" --no-fail-fast "${mtest}" -- --list)
    fi
    listing="$(mktemp)"
    if ! (cd "${REPO_ROOT}" && cargo "${cargo_args[@]}" 2>"${listing}.err" >"${listing}"); then
      echo "  - ${mid}: cargo ${cargo_args[*]} failed" >> "${unresolved_tests}"
      if [[ "${CLAIM_TRACEABILITY_VERBOSE:-0}" = "1" ]]; then
        echo "      stderr:" >> "${unresolved_tests}"
        sed -E 's/^/        /' "${listing}.err" >> "${unresolved_tests}" || true
      fi
      rm -f "${listing}" "${listing}.err"
      continue
    fi
    # `cargo test -- --list` prints lines like:
    #   tests::workspace_index_open_and_query: test
    # The manifest stores the bare function name; accept any entry
    # ending in `::<test_name>` or equal to it exactly.
    if ! awk '/: test$/ { sub(/: test$/, ""); print }' "${listing}" \
        | grep -qE "(^|::)${mtest}\$"; then
      echo "  - ${mid}: cargo ${cargo_args[*]} produced no entry matching '(^|::)${mtest}\$'" >> "${unresolved_tests}"
      if [[ "${CLAIM_TRACEABILITY_VERBOSE:-0}" = "1" ]]; then
        echo "      listing:" >> "${unresolved_tests}"
        sed -E 's/^/        /' "${listing}" >> "${unresolved_tests}" || true
      fi
    fi
    rm -f "${listing}" "${listing}.err"
  done < "${manifest_meta}"
fi

if [[ -s "${unresolved_tests}" ]]; then
  echo "" >&2
  echo "FAIL: manifest test_name(s) do not resolve under per-claim \`cargo test -p <pkg> ... -- --list\`:" >&2
  cat "${unresolved_tests}" >&2
  exit_code=1
fi

if [[ "${exit_code}" -eq 0 ]]; then
  echo "claim-traceability gate: PASS"
fi
exit "${exit_code}"
