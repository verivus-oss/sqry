#!/usr/bin/env bash
#
# File, or update, the issue for one fuzz finding.
#
# Every fuzz workflow used to call `gh issue create` unconditionally with the
# run id in the title. The run id made each title unique, so nothing could ever
# match an existing report and each failing run minted a fresh issue for a
# crash that was already open. Measured on 2026-09-04: 26 open issues covering
# 14 distinct subjects, with svelte_plugin and sql_plugin carrying five each.
#
# This script makes the title a stable key. It looks for an open issue whose
# title matches that key exactly, comments the new run on it when one exists,
# and creates only when none does. The run id lives in the body and the
# comment, where a per-run fact belongs.
#
# The match is exact and case-sensitive against the title, not a GitHub search
# query, which is fuzzy and would collapse distinct targets.
#
# `gh issue list` is not read-your-writes. Measured against
# verivus-oss/sqry on 2026-09-04: an issue took about five seconds to appear
# in the listing, and three back-to-back invocations of this script with one
# title created three issues because each read a listing from before the
# previous create. So a lookup that finds nothing is confirmed by a second
# lookup after a settle delay, and only two empty readings authorize a create.
# That closes the sequential case, which is the one a workflow produces. It
# does not close genuinely simultaneous runs, and nothing available here can:
# issue creation has no idempotency key.

set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/ci/file-fuzz-issue.sh --title <title> --body-file <path>
                                     --comment-file <path>
                                     [--label <csv>] [--match-label <label>]
                                     [--repo <owner/name>] [--limit <n>]

  --title         Stable issue title. Must NOT contain a run id or any other
                  per-run value, or deduplication cannot work.
  --body-file     Body used when creating a new issue.
  --comment-file  Comment posted when an open issue already carries the title.
  --label         Labels for a newly created issue (default: bug,fuzz).
  --match-label   Label the open-issue lookup is narrowed to (default: fuzz).
  --repo          Target repository (default: the gh-resolved repository).
  --limit         Open issues to page through when matching (default: 500).
  --settle-seconds
                  Seconds to wait before the confirming lookup that authorizes
                  a create (default: 10, from the measured listing lag). Only
                  ever paid when the first lookup found nothing.

Prints "created <url>" or "commented <url>" on stdout.

Exit status: 0 filed or commented; 2 argument error; 3 the open-issue listing
was truncated at --limit, so no create is safe.
USAGE
}

title=""
body_file=""
comment_file=""
labels="bug,fuzz"
match_label="fuzz"
repo=""
limit="500"
settle_seconds="${FUZZ_ISSUE_SETTLE_SECONDS:-10}"
# Multiplied by the attempt number between lookup retries. Overridable so the
# behavioural harness can drive the retry path without paying fifteen seconds
# for every mutated lookup; production never sets it.
retry_backoff_seconds="${FUZZ_ISSUE_RETRY_BACKOFF_SECONDS:-5}"

while (($# > 0)); do
  case "$1" in
    --title) title="${2-}"; shift 2 ;;
    --body-file) body_file="${2-}"; shift 2 ;;
    --comment-file) comment_file="${2-}"; shift 2 ;;
    --label) labels="${2-}"; shift 2 ;;
    --match-label) match_label="${2-}"; shift 2 ;;
    --repo) repo="${2-}"; shift 2 ;;
    --limit) limit="${2-}"; shift 2 ;;
    --settle-seconds) settle_seconds="${2-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *)
      echo "FATAL: unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

if [[ -z "$title" || -z "$body_file" || -z "$comment_file" ]]; then
  echo "FATAL: --title, --body-file and --comment-file are all required" >&2
  usage
  exit 2
fi
if [[ ! -r "$body_file" ]]; then
  echo "FATAL: body file is not readable: $body_file" >&2
  exit 2
fi
if [[ ! -r "$comment_file" ]]; then
  echo "FATAL: comment file is not readable: $comment_file" >&2
  exit 2
fi
if [[ ! "$limit" =~ ^[0-9]+$ ]] || ((limit < 1)); then
  echo "FATAL: --limit must be a positive integer, got '$limit'" >&2
  exit 2
fi
if [[ ! "$settle_seconds" =~ ^[0-9]+$ ]]; then
  echo "FATAL: --settle-seconds must be a non-negative integer, got '$settle_seconds'" >&2
  exit 2
fi
if [[ ! "$retry_backoff_seconds" =~ ^[0-9]+$ ]]; then
  echo "FATAL: FUZZ_ISSUE_RETRY_BACKOFF_SECONDS must be a non-negative integer" >&2
  exit 2
fi

# A tab would split the number from the title in the listing below, and a
# newline would split one record into two. Neither can appear in a title a
# caller builds from a target name, so this only ever fires on a caller bug.
case "$title" in
  *$'\t'* | *$'\n'*)
    echo "FATAL: title must not contain a tab or a newline" >&2
    exit 2
    ;;
esac

# A title still carrying a run id defeats the whole point: it can never match a
# previous run's issue. Catch it here rather than discovering it as another
# duplicate a week later.
#
# The pattern is the SHAPE the old titles used, `(run <digits>)`, not "any long
# run of digits". A bare digit-length rule refused a legitimate stable title
# whose target name happened to carry nine digits, which is a false positive
# that would take the workflow down for a real crash (codex, round 1).
if [[ "$title" =~ \(run[[:space:]]+[0-9]+\) || "$title" =~ (^|[^[:alnum:]])run[[:space:]]+[0-9]{6,} ]]; then
  echo "FATAL: title looks like it carries a run id, so it cannot be a stable key: $title" >&2
  exit 2
fi

repo_args=()
if [[ -n "$repo" ]]; then
  repo_args=(--repo "$repo")
fi

# Transient API failures are retried rather than treated as "no match": a
# single failed lookup that fell through to create is how a duplicate gets
# made, which is the defect this script exists to remove.
lookup_ok="false"
existing=""

find_open_issue() {
  local listing=""
  lookup_ok="false"
  existing=""
  # Ask for one MORE row than the caller's limit. Receiving that extra row is
  # the only reliable signal that the page was cut, and it is what separates
  # "exactly at the limit" from "there is more". Comparing rows to the limit
  # itself conflated the two and made the truncation test vacuous: three rows
  # against a limit of three is not a truncated page (codex, round 2).
  local probe_limit=$((limit + 1))
  for attempt in 1 2 3; do
    if listing="$(gh issue list "${repo_args[@]}" \
      --state open \
      --label "$match_label" \
      --limit "$probe_limit" \
      --json number,title \
      --jq '.[] | "\(.number)\t\(.title)"' 2>&1)"; then
      lookup_ok="true"
      break
    fi
    echo "WARN: open-issue lookup attempt ${attempt} failed: ${listing}" >&2
    listing=""
    if ((attempt < 3)); then
      sleep $((attempt * retry_backoff_seconds))
    fi
  done
  [[ "$lookup_ok" == "true" ]] || return 0

  local rows=0
  while IFS=$'\t' read -r number candidate; do
    [[ -n "$number" ]] || continue
    rows=$((rows + 1))
    if [[ "$candidate" == "$title" ]]; then
      # Oldest wins. gh orders by creation descending, so keep overwriting and
      # the last match left standing is the lowest number, which is the issue
      # the deduplication of 2026-09-04 kept.
      existing="$number"
    fi
  done <<<"$listing"

  # gh orders by creation DESCENDING and stops at the limit, so a cut page has
  # dropped the OLDEST issues, which are exactly the ones this lookup wants.
  #
  # The refusal does NOT depend on whether a match was seen. Round 2's version
  # only refused when the page held no match, so a page holding a NEWER
  # duplicate while the oldest was cut away commented on the wrong issue and
  # broke the oldest-wins rule it claims (codex). If the page was cut, this
  # lookup cannot answer, whatever it happened to see.
  #
  # Refusing is the right failure: the run is already failing, and a loud
  # refusal is recoverable where a silent duplicate is the defect being
  # repaired.
  if ((rows > limit)); then
    echo "FATAL: more than ${limit} open '${match_label}' issues exist, so the" >&2
    echo "FATAL: listing was cut. gh orders newest first, so the oldest issues" >&2
    echo "FATAL: were dropped and this lookup cannot tell whether one of them" >&2
    echo "FATAL: already carries this finding. Raise --limit or close some open" >&2
    echo "FATAL: '${match_label}' issues, then re-run." >&2
    lookup_ok="truncated"
    existing=""
  fi
}

find_open_issue
if [[ "$lookup_ok" == "truncated" ]]; then
  exit 3
fi
if [[ "$lookup_ok" == "true" && -z "$existing" ]]; then
  # Nothing found, but the listing is not read-your-writes, so this could be a
  # stale reading of a create that just happened. Let it settle and look again;
  # only two empty readings authorize a create.
  if ((settle_seconds > 0)); then
    sleep "$settle_seconds"
  fi
  find_open_issue
  if [[ "$lookup_ok" == "truncated" ]]; then
    exit 3
  fi
fi

if [[ -n "$existing" ]]; then
  url="$(gh issue comment "$existing" "${repo_args[@]}" --body-file "$comment_file")"
  echo "commented ${url}"
  exit 0
fi

create_body="$body_file"
scratch_body=""
# Every fuzz run that files blind used to leave a file behind in TMPDIR. On a
# long-lived self-hosted runner that accumulates; codex measured 25 leaked per
# harness run in round 1.
cleanup() {
  [[ -n "$scratch_body" && -f "$scratch_body" ]] && rm -f "$scratch_body"
  return 0
}
trap cleanup EXIT

if [[ "$lookup_ok" != "true" ]]; then
  # Three failed lookups. Report the finding rather than drop it, but say in
  # the issue itself that it was filed blind, so a duplicate found later is
  # explained instead of read as this script regressing.
  scratch_body="$(mktemp)"
  create_body="$scratch_body"
  cat "$body_file" >"$create_body"
  {
    echo
    echo "> Deduplication was skipped for this run: three attempts to list the"
    echo "> open \`${match_label}\` issues failed, so this issue was filed without"
    echo "> checking for an existing report. If a duplicate exists, close this one."
  } >>"$create_body"
fi

url="$(gh issue create "${repo_args[@]}" \
  --title "$title" \
  --body-file "$create_body" \
  --label "$labels")"
echo "created ${url}"
