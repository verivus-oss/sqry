#!/usr/bin/env bash
#
# Behavioural test for the fuzz issue filer.
#
# It does not test a copy of the filing logic. It lifts the `File issue on
# crash` run script out of each of the four fuzz workflow YAML files and
# executes that shell against a stateful fake `gh`, so a workflow that stops
# calling the deduplicating filer, or reintroduces the run id into its title,
# fails here.
#
# The central case is the one the 2026-09-04 backlog triage asked for: make a
# target fail twice and end with ONE issue carrying TWO comments, not two
# issues.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The meta-test runs a copy of this script from outside the tree, so it needs
# to say which tree to grade. Nothing else sets this.
REPO_ROOT="${FUZZ_HARNESS_REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# Absolute, and resolved BEFORE the cd: the meta-test copies the harness that
# is RUNNING, and a relative ${BASH_SOURCE[0]} would resolve against
# FUZZ_HARNESS_REPO_ROOT instead, which is how the meta-test came to grade a
# different tree than the one executing (codex, round 3).
HARNESS_SELF="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"
cd "$REPO_ROOT"

# Fail closed on the parser, never skip. This harness proves a claim about the
# shipped workflow YAML; without PyYAML it cannot read that YAML, and a green
# exit would report a check that never ran. Exit class 2 is the capability
# failure, matching scripts/release/check-workflow-run-interpolation.sh.
PYTHON_BIN="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
  echo "FATAL: $PYTHON_BIN is unavailable, so the workflow run scripts cannot be lifted" >&2
  exit 2
fi
if ! "$PYTHON_BIN" -c 'import yaml' 2>/dev/null; then
  echo "FATAL: PyYAML is unavailable, so the workflow run scripts cannot be lifted" >&2
  exit 2
fi

failures=0
checks=0
# Checks per filing workflow, and the fixed workflow-independent checks after
# them. The expected total is derived from the workflow set once it is known,
# so a harness that lifted nothing, or matched no workflow, cannot exit green,
# and the sanitized public mirror (which ships one fuzz workflow, not four)
# still gets an exact figure rather than a lowered bar.
# Per filing workflow: 9 for the lift, the two-run scenario and its payloads,
# 2 for the payload SHAPE across two more runs, 1 for the create labels, then
# KIND_CASE_COUNT for the artifact cases (one each) plus 2 for their titles and
# their count. The kind cases are declared in KIND_CASES below and that
# declaration must have exactly this many members: spelling the number here is
# what stops one edit removing a case and its expectation together (codex,
# round 7).
KIND_CASE_COUNT=5
CHECKS_PER_WORKFLOW=$((9 + 2 + 1 + KIND_CASE_COUNT + 2))
FIXED_CHECKS=106
expected_checks=0

pass() { checks=$((checks + 1)); printf 'ok\t%s\n' "$1"; }
fail() {
  checks=$((checks + 1))
  failures=$((failures + 1))
  printf 'FAIL\t%s\n' "$1" >&2
}
assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    pass "$label"
  else
    fail "$label: expected '$expected', got '$actual'"
  fi
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# --------------------------------------------------------------------------
# Fake gh. Per-repository state: one TSV of issues and one of comments under a
# bucket named for the effective --repo, so a second invocation in the same
# scenario sees what the first one wrote and a call aimed elsewhere does not.
#
# It models the real listing lag, because the first version of this harness did
# not and passed a filer that then created three issues for one title against
# the live API. GH_FAKE_LIST_LAG hides each issue from that many subsequent
# list calls, which is what `gh issue list` does for about five seconds after a
# create.
#
# Every invocation is serialized to calls.jsonl as a JSON argv ARRAY before the
# fake does anything else, and a result record follows it. Recording `$*` after
# the simulated failures lost both the argument boundaries and every call that
# failed (codex, round 6).
# --------------------------------------------------------------------------
mkdir -p "$WORK/bin"

# The slug, the bucket layout and the encodings are shared between the fake and
# the assertions that read what it recorded, so the two cannot drift on where
# state lives or on how a value was written down.
cat >"$WORK/bin/gh-fake-lib.sh" <<'FAKELIB'
# gh resolves every call against ONE repository. Modelling the world as a
# single global issue list meant a filer that pointed its writes at a different
# repository still wrote into the only bucket there was, so the mutation passed
# (codex, round 6). State is keyed by the effective --repo instead.
gh_fake_slug() {
  local slug="${1:-__default__}"
  [[ -n "$slug" ]] || slug="__default__"
  slug="${slug//\//__}"
  slug="${slug//[^A-Za-z0-9_.-]/_}"
  printf '%s' "$slug"
}
gh_fake_bucket() {  # state [repo]
  printf '%s/repo-%s' "$1" "$(gh_fake_slug "${2-}")"
}

# JSON, so argument boundaries survive into the assertions. Recording `$*`
# joined them with spaces, which made a single argument spelled
# `octo/wrong --repo verivus-oss/sqry` indistinguishable from two real
# arguments, and the assertion that read it back could not tell (codex,
# round 6).
gh_fake_json_string() {
  local s="${1-}" out='"' i n ch code esc
  n=${#s}
  for ((i = 0; i < n; i++)); do
    ch="${s:i:1}"
    case "$ch" in
      '"')   out+='\"' ;;
      '\')   out+='\\' ;;
      $'\n') out+='\n' ;;
      $'\r') out+='\r' ;;
      $'\t') out+='\t' ;;
      *)
        printf -v code '%d' "'$ch"
        if ((code >= 0 && code < 32)); then
          printf -v esc '\\u%04x' "$code"
          out+="$esc"
        else
          out+="$ch"
        fi
        ;;
    esac
  done
  printf '%s"' "$out"
}
gh_fake_json_array() {  # values... -> a JSON array
  local out='[' first=1 v
  for v in "$@"; do
    ((first)) || out+=','
    first=0
    out+="$(gh_fake_json_string "$v")"
  done
  printf '%s]' "$out"
}

# gh's --json is a pflag StringSlice: it ACCUMULATES across repetitions and
# CSV-decodes each occurrence. Keeping only the last one meant
# `--json notAField --json number,title` passed the whole suite while live gh
# rejects the call outright, three retries exhaust and the filer blind-creates
# on every run (codex and grok, round 6). Decoding matters independently:
# without it a member spelled `"number"` is false-red.
#
# ONE reader, in gh_fake_csv.py beside this file, shared with the assertions
# that read the call log. Round 6's repair wrote the grammar twice, in bash
# here and in Python there, and both were wrong: both accepted characters
# after a closing quote, so `"num"ber,title` decoded to a legal projection
# while live gh 2.90.0 refuses the call, and they disagreed on an unterminated
# quote, so the reader could manufacture the exact field names its assertions
# expected (codex, round 7). Two copies of one grammar is one more than can be
# kept honest.
# Sets GH_FAKE_CSV_FIELDS; on a parse error sets GH_FAKE_CSV_ERROR to gh's
# message and returns non-zero.
gh_fake_csv_decode() {  # record -> GH_FAKE_CSV_FIELDS
  GH_FAKE_CSV_FIELDS=()
  GH_FAKE_CSV_ERROR=""
  local scratch="${GH_FAKE_STATE:?}/csv.$$"
  if ! python3 "$(dirname "${BASH_SOURCE[0]}")/gh_fake_csv.py" "${1-}" >"$scratch"; then
    GH_FAKE_CSV_ERROR="$(cat "$scratch")"
    rm -f "$scratch"
    return 1
  fi
  mapfile -d '' -t GH_FAKE_CSV_FIELDS <"$scratch"
  rm -f "$scratch"
}

# gh's own repository rule, `[HOST/]OWNER/REPO`: two or three non-empty parts.
# Asking only for a slash accepted `cli/` and `cli/cli/extra/path`, both of
# which live gh refuses (grok, round 7): the fake accepting what gh refuses is
# the dangerous direction.
gh_fake_repo_ok() {
  local repo="$1" parts
  IFS='/' read -r -a parts <<<"$repo"
  ((${#parts[@]} == 2 || ${#parts[@]} == 3)) || return 1
  local part
  for part in "${parts[@]}"; do [[ -n "$part" ]] || return 1; done
  [[ "$repo" != */ ]]
}
FAKELIB

cat >"$WORK/bin/gh_fake_csv.py" <<'FAKECSV'
"""Go's encoding/csv on one record, which is what pflag runs a StringSlice
value through.  The single implementation for the fake gh and for the log
reader; see gh_fake_csv_decode for why there is only one.

Measured against gh 2.90.0 rather than assumed (codex and grok, round 7):
  number,title          -> [number, title]
  "number","title"      -> [number, title]
  number,               -> [number, ""]      gh: Unknown JSON field: ""
  "num"ber,title        -> parse error: extraneous or missing " in quoted-field
  number,"title         -> parse error: extraneous or missing " in quoted-field
  num"ber               -> parse error: bare " in non-quoted-field
  (empty)               -> []                pflag short-circuits before csv
The column in the message is the offending character's 1-based position; Go
counts slightly differently and nothing asserts on the number.
"""
import sys


class CsvError(ValueError):
    pass


def read_as_csv(value):
    if value == "":
        return []
    # One csv.Reader.Read(): blank lines before the record are skipped, and
    # the record ends at the first unquoted newline, with a preceding CR
    # dropped. Nothing after that terminator is read. `number,title\n` was
    # therefore refused here and accepted live (false-red), while a value of
    # only `\n` was accepted here and is an EOF error live (false-green)
    # (grok and codex, round 8).
    while value.startswith("\n") or value.startswith("\r\n"):
        value = value[2:] if value.startswith("\r\n") else value[1:]
    if value == "":
        raise CsvError("EOF")
    fields = []
    i, n = 0, len(value)
    while True:
        if i < n and value[i] == '"':
            i += 1
            field = []
            while True:
                if i >= n:
                    raise CsvError(
                        f'parse error on line 1, column {n + 1}: '
                        'extraneous or missing " in quoted-field')
                ch = value[i]
                if ch == '"':
                    if i + 1 < n and value[i + 1] == '"':
                        field.append('"')
                        i += 2
                        continue
                    i += 1
                    if i < n and value[i] not in (",", "\n", "\r"):
                        raise CsvError(
                            f'parse error on line 1, column {i + 1}: '
                            'extraneous or missing " in quoted-field')
                    break
                if ch == "\r" and i + 1 < n and value[i + 1] == "\n":
                    # CRLF inside a quoted field reads as LF, as Go does.
                    field.append("\n")
                    i += 2
                    continue
                field.append(ch)
                i += 1
            fields.append("".join(field))
        else:
            j = i
            while j < n and value[j] not in (",", "\n", "\r"):
                if value[j] == '"':
                    raise CsvError(
                        f'parse error on line 1, column {j + 1}: '
                        'bare " in non-quoted-field')
                j += 1
            fields.append(value[i:j])
            i = j
        if i < n and value[i] == ",":
            i += 1
            continue
        # A record terminator (or the end) closes the record. Whatever
        # follows a newline is a second record pflag never reads.
        break
    return fields


if __name__ == "__main__":
    try:
        for member in read_as_csv(sys.argv[1] if len(sys.argv) > 1 else ""):
            sys.stdout.write(member + "\0")
    except CsvError as error:
        sys.stdout.write(str(error))
        sys.exit(1)
FAKECSV

cat >"$WORK/bin/gh" <<'FAKE'
#!/usr/bin/env bash
#
# Fake gh. It PARSES the flags it is given rather than ignoring them, because a
# fake that ignores flags cannot fail when the subject stops passing one. Round
# 1 of review demonstrated that: dropping --json and --jq from the real lookup
# survived the whole suite, and against live gh that change returns colorized
# JSON instead of the TSV the filer parses, so every run would have created.
#
# Modelled on gh 2.90.0: `gh issue list` orders by creation DESCENDING and
# stops when --limit is reached, so a truncated page drops the OLDEST issues.
#
# Every invocation is written down as JSON before anything else happens, so a
# call that is about to be refused, or about to fail, is still evidence. The
# round-5 fake logged after its simulated failures, so a failed lookup left no
# trace at all and nothing could assert on it (codex, round 6).
set -euo pipefail
# shellcheck source=/dev/null
source "$(dirname "${BASH_SOURCE[0]}")/gh-fake-lib.sh"

state="${GH_FAKE_STATE:?GH_FAKE_STATE unset}"
mkdir -p "$state"
calls="$state/calls.jsonl"
touch "$calls"

CALL_SEQ=0
CALL_REPO=""
CALL_BODY_FILE=""
CALL_STDOUT=""
CALL_ISSUE=""

record_call() {
  local seq_file="$state/seq" seq=0
  [[ -f "$seq_file" ]] && seq="$(cat "$seq_file")"
  seq=$((seq + 1))
  printf '%s' "$seq" >"$seq_file"
  CALL_SEQ=$seq
  printf '{"record":"call","seq":%s,"argv":%s}\n' \
    "$seq" "$(gh_fake_json_array "$@")" >>"$calls"
}

# The result record carries what the call actually did: its status, the
# repository it resolved against, the issue it touched, and the BODY BYTES it
# was handed. Asserting on a body file after the run cannot see a body that was
# written, read and deleted, which is what the workflow's cleanup trap does.
finish_call() {
  local status="$1" body="" out=""
  set +e +u
  if [[ -n "$CALL_BODY_FILE" && -r "$CALL_BODY_FILE" ]]; then
    body="$(cat "$CALL_BODY_FILE")"
  fi
  out="$CALL_STDOUT"
  printf '{"record":"result","seq":%s,"status":%s,"repo":%s,"issue":%s,"body":%s,"stdout":%s}\n' \
    "$CALL_SEQ" "$status" \
    "$(gh_fake_json_string "$CALL_REPO")" \
    "$(gh_fake_json_string "$CALL_ISSUE")" \
    "$(gh_fake_json_string "$body")" \
    "$(gh_fake_json_string "$out")" >>"$calls"
  return 0
}
trap 'finish_call "$?"' EXIT

die() { echo "fake gh: $*" >&2; exit 1; }

record_call "$@"

# --repo is a plain string flag: it does NOT accumulate, the last one wins.
# The bucket is chosen from the parsed value, not from a scan of the raw words,
# so a value that merely CONTAINS the flag spelling cannot select it.
open_bucket() {
  CALL_REPO="$1"
  bucket="$(gh_fake_bucket "$state" "$1")"
  mkdir -p "$bucket"
  issues="$bucket/issues.tsv"
  comments="$bucket/comments.tsv"
  touch "$issues" "$comments"
}

case "${1:-}:${2:-}" in
  issue:list)
    want_state="open"; want_labels=(); want_limit=30; want_jq=""
    want_repo=""; json_seen=""; json_fields=()
    shift 2
    while (($# > 0)); do
      case "$1" in
        --state|-s) want_state="$2"; shift 2 ;;
        # --label is a StringSlice too, so it accumulates AND CSV-decodes.
        --label|-l)
          gh_fake_csv_decode "$2" \
            || die "invalid argument \"$2\" for \"--label\" flag: $GH_FAKE_CSV_ERROR"
          want_labels+=(${GH_FAKE_CSV_FIELDS[@]+"${GH_FAKE_CSV_FIELDS[@]}"})
          shift 2
          ;;
        --limit|-L) want_limit="$2"; shift 2 ;;
        --jq|-q)    want_jq="$2";   shift 2 ;;
        --repo|-R)  want_repo="$2"; shift 2 ;;
        --json)
          json_seen=1
          gh_fake_csv_decode "$2" \
            || die "invalid argument \"$2\" for \"--json\" flag: $GH_FAKE_CSV_ERROR"
          json_fields+=(${GH_FAKE_CSV_FIELDS[@]+"${GH_FAKE_CSV_FIELDS[@]}"})
          shift 2
          ;;
        # Real gh accepts the equals form for every long flag. Refusing it made
        # the fake false-red for a spelling the filer could legally move to.
        --state=*) want_state="${1#*=}"; shift ;;
        --label=*)
          gh_fake_csv_decode "${1#*=}" \
            || die "invalid argument \"${1#*=}\" for \"--label\" flag: $GH_FAKE_CSV_ERROR"
          want_labels+=(${GH_FAKE_CSV_FIELDS[@]+"${GH_FAKE_CSV_FIELDS[@]}"})
          shift
          ;;
        --limit=*) want_limit="${1#*=}"; shift ;;
        --jq=*)    want_jq="${1#*=}";    shift ;;
        --repo=*)  want_repo="${1#*=}"; shift ;;
        --json=*)
          json_seen=1
          gh_fake_csv_decode "${1#*=}" \
            || die "invalid argument \"${1#*=}\" for \"--json\" flag: $GH_FAKE_CSV_ERROR"
          json_fields+=(${GH_FAKE_CSV_FIELDS[@]+"${GH_FAKE_CSV_FIELDS[@]}"})
          shift
          ;;
        *) die "unsupported issue list flag: $1" ;;
      esac
    done
    [[ -z "$want_repo" ]] || gh_fake_repo_ok "$want_repo" \
      || die "expected the \"[HOST/]OWNER/REPO\" format, got \"$want_repo\""
    open_bucket "$want_repo"

    if [[ "${GH_FAKE_LIST_FAILS:-0}" == "1" ]]; then
      die "simulated API failure"
    fi
    if [[ -n "${GH_FAKE_LIST_FAIL_TIMES:-}" ]]; then
      n_file="$state/list-failures"
      seen=0; [[ -f "$n_file" ]] && seen="$(cat "$n_file")"
      if ((seen < GH_FAKE_LIST_FAIL_TIMES)); then
        printf '%s' "$((seen + 1))" >"$n_file"
        die "simulated transient API failure $((seen + 1))"
      fi
    fi

    # What real gh 2.90.0 does, measured, not assumed:
    #   no --json                -> a TSV TABLE: number, state, title, labels, ts
    #   --json X without --jq    -> a JSON array
    #   --json X with the --jq   -> the rendered TSV
    # The round-2 fake called the first case "JSON", which was simply wrong
    # (grok). It is refused here because the filer's read loop would silently
    # mis-parse a five-column table as number-plus-title, not because gh
    # errors. Flag PRESENCE and the projection being non-empty are separate
    # conditions: `--json=` is accepted by pflag and yields an empty slice,
    # which gh then refuses for a different reason and with a different
    # message (codex, round 6).
    if [[ -z "$json_seen" ]]; then
      die "issue list without --json returns gh's five-column table, which this caller mis-parses"
    fi
    # An EMPTY projection is accepted. `--json=` and `--json ''` return `[{}]`
    # per issue on live 2.90.0, exit 0, and the TSV --jq then renders
    # `null<TAB>null` for each. The round-6 fake refused both with the message
    # gh prints only for `--json` given no argument at all, which was the
    # over-fix grok warned about in round 5 (grok, round 7). Fidelity is the
    # fake's job; requiring the filer to ask for number and title is the
    # harness's, and it does, by asserting the projection it sent.
    # gh 2.90.0 rejects an unknown field outright, `Unknown JSON field: "x"`.
    # The round-3 fake accepted any projection merely CONTAINING number and
    # title, so mutating the filer to a live-illegal projection survived the
    # whole suite at 93/0 while against real gh every lookup would fail, the
    # three retries would exhaust, and the filer would blind-create on every
    # run: the original defect back, under a green instrument. Codex and grok
    # found this independently in round 3, both against live gh. The list is
    # `gh issue list --json` with no value, on 2.90.0.
    # Compared by EQUALITY over decoded members. A space-joined string matched
    # by substring accepted `assignees author` as one field (codex, round 5);
    # keeping only the last occurrence accepted an illegal field beside a legal
    # one (codex and grok, round 6). That is three false-greens in this one
    # validator, which is why it is now the decoded slice gh itself would build.
    gh_issue_list_fields=(
      assignees author body closed closedAt closedByPullRequestsReferences
      comments createdAt id isPinned labels milestone number projectCards
      projectItems reactionGroups state stateReason title updatedAt url
    )
    for want_field in "${json_fields[@]}"; do
      want_field_known=""
      for known_field in "${gh_issue_list_fields[@]}"; do
        if [[ "$want_field" == "$known_field" ]]; then
          want_field_known=1
          break
        fi
      done
      [[ -n "$want_field_known" ]] || die "Unknown JSON field: \"$want_field\""
    done
    # A field the projection did not ask for renders as jq's `null`, which is
    # what live gh does, rather than as a refusal, which is what the round-6
    # fake did. A filer that drops `title` then matches nothing and creates
    # every run, and the behavioural checks see that; the fake does not need
    # to editorialise.
    render_number="null"; render_title="null"
    for want_field in ${json_fields[@]+"${json_fields[@]}"}; do
      [[ "$want_field" == "number" ]] && render_number="value"
      [[ "$want_field" == "title" ]] && render_title="value"
    done
    if [[ "$want_jq" != '.[] | "\(.number)\t\(.title)"' ]]; then
      die "issue list with --json but without the TSV --jq returns a JSON array"
    fi
    case "$want_state" in
      open|closed|all) ;;
      *) die "invalid argument \"$want_state\" for \"--state\": valid values are {open|closed|all}" ;;
    esac
    lag="${GH_FAKE_LIST_LAG:-0}"
    emitted=0
    # Creation-descending, matching gh 2.90.0.
    while IFS=$'\t' read -r number title labels issue_state; do
      [[ -n "$number" ]] || continue
      [[ "$want_state" == "all" || "$issue_state" == "$want_state" ]] || continue
      # Repeated --label is AND on live gh: `--label fuzz --label nope`
      # returned 0 rows where `--label fuzz` returned 10. The fake took the
      # last one and would have matched an issue live could not see, so a
      # filer that moved to two labels would comment where production creates
      # (grok, round 3). Latent today, false-green tomorrow.
      label_miss=""
      for want_label in ${want_labels[@]+"${want_labels[@]}"}; do
        case ",$labels," in *",$want_label,"*) ;; *) label_miss=1; break ;; esac
      done
      [[ -z "$label_miss" ]] || continue
      if ((lag > 0)); then
        seen_file="$bucket/listed-$number"
        seen=0
        [[ -f "$seen_file" ]] && seen="$(cat "$seen_file")"
        printf '%s' "$((seen + 1))" >"$seen_file"
        ((seen < lag)) && continue
      fi
      # Wall-clock lag, which is what real gh actually has. The call-count
      # model above cannot tell a filer that waits ten seconds from one that
      # waits two, so a hardcoded `sleep 2` passed the whole suite while being
      # shorter than the measured five-second lag (grok, round 2).
      if ((${GH_FAKE_LIST_LAG_SECONDS:-0} > 0)); then
        created_file="$bucket/created-$number"
        if [[ -f "$created_file" ]]; then
          age=$(( $(date +%s) - $(cat "$created_file") ))
          ((age < GH_FAKE_LIST_LAG_SECONDS)) && continue
        fi
      fi
      ((emitted < want_limit)) || break
      emitted=$((emitted + 1))
      [[ "$render_number" == "value" ]] || number="null"
      [[ "$render_title" == "value" ]] || title="null"
      CALL_STDOUT+="$number"$'\t'"$title"$'\n'
    done < <(tac "$issues")
    printf '%s' "$CALL_STDOUT"
    ;;
  issue:create)
    title=""; create_labels=(); body_file=""; want_repo=""
    shift 2
    while (($# > 0)); do
      case "$1" in
        --title|-t)      title="$2"; shift 2 ;;
        --body-file|-F)  body_file="$2"; shift 2 ;;
        --label|-l)
          gh_fake_csv_decode "$2" \
            || die "invalid argument \"$2\" for \"--label\" flag: $GH_FAKE_CSV_ERROR"
          create_labels+=(${GH_FAKE_CSV_FIELDS[@]+"${GH_FAKE_CSV_FIELDS[@]}"})
          shift 2
          ;;
        --repo|-R)       want_repo="$2"; shift 2 ;;
        --title=*)       title="${1#*=}"; shift ;;
        --body-file=*)   body_file="${1#*=}"; shift ;;
        --label=*)
          gh_fake_csv_decode "${1#*=}" \
            || die "invalid argument \"${1#*=}\" for \"--label\" flag: $GH_FAKE_CSV_ERROR"
          create_labels+=(${GH_FAKE_CSV_FIELDS[@]+"${GH_FAKE_CSV_FIELDS[@]}"})
          shift
          ;;
        --repo=*)        want_repo="${1#*=}"; shift ;;
        *) die "unsupported issue create flag: $1" ;;
      esac
    done
    [[ -z "$want_repo" ]] || gh_fake_repo_ok "$want_repo" \
      || die "expected the \"[HOST/]OWNER/REPO\" format, got \"$want_repo\""
    open_bucket "$want_repo"
    [[ -n "$title" ]] || die "issue create with no title"
    [[ -n "$body_file" && -r "$body_file" ]] || die "issue create with no readable body file"
    CALL_BODY_FILE="$body_file"
    # --label accumulates on create too, so two of them apply two labels.
    labels=""
    for one_label in ${create_labels[@]+"${create_labels[@]}"}; do
      labels="${labels:+$labels,}$one_label"
    done
    number="$(($(wc -l <"$issues") + 1000))"
    CALL_ISSUE="$number"
    printf '%s\t%s\t%s\t%s\n' "$number" "$title" "$labels" "open" >>"$issues"
    date +%s >"$bucket/created-$number"
    cp "$body_file" "$bucket/body-$number.md"
    CALL_STDOUT="https://example.invalid/issues/$number"
    printf '%s\n' "$CALL_STDOUT"
    ;;
  issue:comment)
    body_file=""; want_repo=""; number=""
    shift 2
    while (($# > 0)); do
      case "$1" in
        --body-file|-F) body_file="$2"; shift 2 ;;
        --repo|-R)      want_repo="$2"; shift 2 ;;
        --body-file=*)  body_file="${1#*=}"; shift ;;
        --repo=*)       want_repo="${1#*=}"; shift ;;
        -*) die "unsupported issue comment flag: $1" ;;
        *)
          [[ -z "$number" ]] || die "issue comment with two selectors: $number and $1"
          number="$1"; shift
          ;;
      esac
    done
    [[ -z "$want_repo" ]] || gh_fake_repo_ok "$want_repo" \
      || die "expected the \"[HOST/]OWNER/REPO\" format, got \"$want_repo\""
    open_bucket "$want_repo"
    [[ -n "$number" ]] || die "issue comment with no issue selector"
    CALL_ISSUE="$number"
    [[ -n "$body_file" && -r "$body_file" ]] || die "issue comment with no readable body file"
    CALL_BODY_FILE="$body_file"
    grep -q "^${number}"$'\t' "$issues" \
      || die "comment on an issue that does not exist in this repository: $number"
    printf '%s\n' "$number" >>"$comments"
    cat "$body_file" >>"$bucket/comment-$number.md"
    CALL_STDOUT="https://example.invalid/issues/$number#comment"
    printf '%s\n' "$CALL_STDOUT"
    ;;
  *)
    die "unexpected invocation: $*"
    ;;
esac
FAKE
chmod +x "$WORK/bin/gh"
# shellcheck source=/dev/null
source "$WORK/bin/gh-fake-lib.sh"

# --------------------------------------------------------------------------
# Reading the call log back, with gh's own option semantics.
#
# The round-5 assertions grepped a space-joined line. That could not tell one
# argument from two, so `--repo "octo/wrong --repo $repo"` read as a correct
# forward; and it could not model a flag that ACCUMULATES, so an illegal
# `--json` beside a legal one read as legal (codex, round 6). Both are arity
# questions, so arity is modelled once, here, and every assertion asks this.
# --------------------------------------------------------------------------
cat >"$WORK/gh-calls.py" <<'CALLSQ'
#!/usr/bin/env python3
"""Query the fake gh call log using the flag arity gh itself gives each flag.

--json and --label are pflag StringSlices: they accumulate across repetitions
and CSV-decode each occurrence.  Everything else is a plain string or int flag
where the last occurrence wins.  Getting this wrong in the assertions is what
let two round-6 mutations through.
"""
import json
import os
import sys

# The same reader the fake uses. A second copy of the grammar here decoded
# `"num"ber,title` to a legal projection and accepted an unterminated quote
# the fake refused (codex, round 7); it could not disagree with the fake and
# be right, so it is gone.
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "bin"))
from gh_fake_csv import CsvError, read_as_csv  # noqa: E402

CANON = {
    "-R": "--repo", "-s": "--state", "-q": "--jq", "-L": "--limit",
    "-t": "--title", "-F": "--body-file", "-l": "--label",
}
SLICE_FLAGS = {"--json", "--label"}
VALUE_FLAGS = {
    "--repo", "--state", "--jq", "--limit", "--title", "--body-file",
    "--json", "--label",
}
ABSENT = "<absent>"
INVALID = "<invalid-csv>"


def parse(argv, skip):
    """-> (options, positionals). Slice flags keep every member, in order."""
    opts = {}
    positional = []
    i = skip
    while i < len(argv):
        arg = argv[i]
        name = value = None
        if arg.startswith("--") and "=" in arg:
            name, value = arg.split("=", 1)
        elif CANON.get(arg, arg) in VALUE_FLAGS:
            name = arg
            value = argv[i + 1] if i + 1 < len(argv) else ""
            i += 1
        if name is None:
            positional.append(arg)
            i += 1
            continue
        name = CANON.get(name, name)
        if name in SLICE_FLAGS:
            # A refused call is in the log too, so its value may not parse.
            # That is recorded as a marker, never as a guess at the fields.
            try:
                members = read_as_csv(value)
            except CsvError:
                members = [INVALID]
            opts.setdefault(name, []).extend(members)
        else:
            opts[name] = value
        i += 1
    return opts, positional


def load(path):
    """-> [call], each with its result record merged in."""
    calls, results = [], {}
    # A refusal before the first gh call leaves no log at all; that is zero
    # calls, not an error, and it is exactly the case the refusal checks read.
    if not os.path.exists(path):
        return calls
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            record = json.loads(line)
            if record["record"] == "call":
                calls.append(record)
            else:
                results[record["seq"]] = record
    for call in calls:
        call["result"] = results.get(call["seq"], {})
        call["sub"] = " ".join(call["argv"][:2])
        call["opts"], call["positional"] = parse(call["argv"], 2)
    return calls


def select(calls, sub):
    return [c for c in calls if c["sub"] == sub] if sub != "*" else calls


def main():
    path, selector = sys.argv[1], sys.argv[2]
    rest = sys.argv[3:]
    calls = load(path)

    if selector == "count":
        print(len(select(calls, rest[0])))
    elif selector == "subs":
        # Trailing comma on each, so an empty set reads as empty and a
        # one-element set cannot be mistaken for a prefix of a larger one.
        print("".join(s + "," for s in sorted({c["sub"] for c in calls})))
    elif selector == "optmembers":
        # One decoded member per line, so membership is tested by EQUALITY.
        # Joining them back into a comma string reintroduced exactly the
        # ambiguity the decoding removed.
        sub, index, name = rest[0], int(rest[1]), rest[2]
        chosen = select(calls, sub)
        if chosen:
            value = chosen[index]["opts"].get(name, None)
            if value is None:
                print(ABSENT)
            else:
                for member in (value if isinstance(value, list) else [value]):
                    print(member)
    elif selector in ("opt", "optall"):
        # opt <sub> <index> <name>; optall <sub> <name>
        if selector == "opt":
            sub, index, name = rest[0], int(rest[1]), rest[2]
        else:
            sub, index, name = rest[0], None, rest[1]
        chosen = select(calls, sub)
        if index is not None:
            chosen = [chosen[index]] if chosen else []
        for call in chosen:
            value = call["opts"].get(name, None)
            if value is None:
                print(ABSENT)
            elif isinstance(value, list):
                print(",".join(value))
            else:
                print(value)
    elif selector == "positional":
        for call in select(calls, rest[0]):
            print(" ".join(call["positional"]))
    elif selector == "status":
        for call in select(calls, rest[0]):
            print(call["result"].get("status", ABSENT))
    elif selector == "repo":
        for call in select(calls, rest[0]):
            print(call["result"].get("repo", ABSENT))
    elif selector == "issue":
        for call in select(calls, rest[0]):
            print(call["result"].get("issue", ABSENT))
    elif selector in ("body", "stdout"):
        sub, index = rest[0], int(rest[1])
        chosen = select(calls, sub)
        sys.stdout.write(chosen[index]["result"].get(selector, "") if chosen else "")
    elif selector == "argv":
        chosen = select(calls, rest[0])
        print(json.dumps(chosen[int(rest[1])]["argv"]))
    else:
        raise SystemExit("unknown selector: " + selector)


main()
CALLSQ

# Assertions ask through these, never by grepping the log.
gh_calls() {  # state selector args...
  local state="$1"; shift
  python3 "$WORK/gh-calls.py" "$state/calls.jsonl" "$@"
}

# --------------------------------------------------------------------------
# Lift the `File issue on crash` run script and its env out of a workflow.
# --------------------------------------------------------------------------
extract_step() {
  local workflow="$1" out_script="$2" out_env="$3"
  "$PYTHON_BIN" - "$workflow" "$out_script" "$out_env" <<'PY'
import pathlib, sys, yaml

workflow, out_script, out_env = sys.argv[1:4]
data = yaml.safe_load(pathlib.Path(workflow).read_text())
found = []
for job in data["jobs"].values():
    for step in job.get("steps", []):
        if step.get("name") == "File issue on crash":
            found.append(step)
if len(found) != 1:
    raise SystemExit(
        f"{workflow}: expected exactly one 'File issue on crash' step, found {len(found)}"
    )
step = found[0]
run = step["run"]
if "${{" in run:
    raise SystemExit(f"{workflow}: run body interpolates a workflow expression; cannot lift it")
if "scripts/ci/file-fuzz-issue.sh" not in run:
    raise SystemExit(f"{workflow}: filing step does not call the deduplicating filer")
if "gh issue create" in run:
    raise SystemExit(f"{workflow}: filing step still calls gh issue create directly")
pathlib.Path(out_script).write_text(run)
# The step's `if:` is recorded beside the shell. The harness lifts the shell
# body and cannot see whether Actions would run it, and this file said so as
# residue; cursor showed in round 8 that the condition is already parsed here
# and simply never looked at, so `if: success()` (file on every green run)
# passed at 179/0. Recorded, and pinned where the workflow is lifted.
pathlib.Path(out_script + ".if").write_text(str(step.get("if", "")))
# Only the names matter here; the values come from the scenario.
# Name AND declared value. Keeping only the keys meant the harness supplied
# its own value for every name, so pinning `TARGET: ${{ matrix.target }}` to a
# constant survived at 107/0 while in production every matrix target would
# collapse onto one issue (codex, round 5).
declared_env = step.get("env", {})
pathlib.Path(out_env).write_text(
    "".join(f"{name}\t{declared_env[name]}\n" for name in sorted(declared_env))
)
PY
}

# --------------------------------------------------------------------------
# Scenario driver. Runs the lifted step N times against one fake-gh state.
# --------------------------------------------------------------------------
# The whole grammar of workflow expressions this harness understands: the
# context references these four workflows actually use, and nothing else.
#
# Values are resolved THROUGH the declared expression. Keying them off the env
# NAME meant a workflow that declared `TARGET: ${{ 'svelte_plugin' }}` was
# still handed the per-scenario target, so every matrix target collapsing onto
# one issue stayed invisible; the shape test that was supposed to catch it only
# asked whether the value contained `${{` and `}}`, which that spelling does,
# and so does `${{ matrix.target && 'svelte_plugin' }}` (codex, round 6).
# An expression outside the grammar is a hard error, never a guess: a harness
# that invents a value cannot fail about the declaration it invented it for.
resolve_workflow_expression() {  # value run-id target crate
  local value="$1" run_id="$2" target="$3" crate="$4"
  local out="" rest="$value" before expression
  while [[ "$rest" == *'${{'* ]]; do
    before="${rest%%'${{'*}"
    rest="${rest#*'${{'}"
    [[ "$rest" == *'}}'* ]] || return 1
    expression="${rest%%'}}'*}"
    rest="${rest#*'}}'}"
    expression="${expression#"${expression%%[![:space:]]*}"}"
    expression="${expression%"${expression##*[![:space:]]}"}"
    out+="$before"
    case "$expression" in
      matrix.target)        out+="$target" ;;
      matrix.crate)         out+="$crate" ;;
      github.run_id)        out+="$run_id" ;;
      github.server_url)    out+="$WORKFLOW_SERVER_URL" ;;
      github.repository)    out+="$WORKFLOW_REPOSITORY" ;;
      secrets.GITHUB_TOKEN) out+="fake-token" ;;
      github.token)         out+="fake-token" ;;
      *) return 1 ;;
    esac
  done
  printf '%s' "$out$rest"
}
WORKFLOW_SERVER_URL="https://example.invalid"
WORKFLOW_REPOSITORY="verivus-oss/sqry"

# The lifted step runs with EXACTLY the environment its YAML declares, and no
# more. Supplying TARGET unconditionally meant deleting `TARGET: ${{ ... }}`
# from the workflow passed at 103/0, while the real step would die under
# `set -u` with `TARGET: unbound variable` (codex, round 4). extract_step had
# been recording the declared names all along and nothing read the file.
#
# `env -i`, because plain `env` ADDS to the caller's environment instead of
# replacing it. An ambient TARGET reached the child, so the comment above was
# false and a workflow reading an undeclared variable would have passed here
# and died in production (codex, round 6).
run_step() {
  local script="$1" state="$2" run_id="$3" target="${4:-}" crate="${5:-}"
  local env_file="${script%.sh}.env"
  local -a step_env=()
  local name value resolved
  while IFS=$'\t' read -r name value; do
    [[ -n "$name" ]] || continue
    if ! resolved="$(resolve_workflow_expression "$value" "$run_id" "$target" "$crate")"; then
      printf 'FATAL\t%s declares %s as %s, which this harness cannot resolve\n' \
        "$script" "$name" "$value" >&2
      return 1
    fi
    step_env+=("$name=$resolved")
  done <"$env_file"
  (
    cd "$WORK/tree"
    env -i \
      "PATH=$WORK/bin:$PATH" \
      ${TMPDIR:+"TMPDIR=$TMPDIR"} \
      "GH_FAKE_STATE=$state" \
      "GH_FAKE_LIST_LAG=${GH_FAKE_LIST_LAG:-0}" \
      FUZZ_ISSUE_SETTLE_SECONDS=0 \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 \
      "${step_env[@]}" \
      bash "$script"
  )
}

# Every one of these addresses ONE repository's state. The default bucket is
# whatever gh resolves when no --repo is given, which is what all four fuzz
# workflows rely on; a scenario that passes --repo names its bucket instead.
issues_file()   { printf '%s/issues.tsv'   "$(gh_fake_bucket "$1" "${2-}")"; }
comments_file() { printf '%s/comments.tsv' "$(gh_fake_bucket "$1" "${2-}")"; }
count_issues() {  # state [repo]
  local file; file="$(issues_file "$1" "${2-}")"
  [[ -f "$file" ]] || { echo 0; return; }
  wc -l <"$file" | tr -d ' '
}
seed_issue() {  # state number title labels open|closed [repo]
  local file; file="$(issues_file "$1" "${6-}")"
  mkdir -p "$(dirname "$file")"
  printf '%s\t%s\t%s\t%s\n' "$2" "$3" "$4" "$5" >>"$file"
}
# The run this scenario is, spelled the way the workflow's own declarations
# spell it. Independent of the subject: nothing here is read back out of the
# filer or the workflow, so a payload cannot agree with itself.
expected_run_url() { printf '%s/%s/actions/runs/%s' "$WORKFLOW_SERVER_URL" "$WORKFLOW_REPOSITORY" "$1"; }
# Two populations, each taken WHOLE. Every URL token, to whitespace, must be
# the expected run URL exactly: the round-7 pattern stopped at `[0-9]+`, so
# `.../111111111x` matched on its correct prefix and the trailing character
# was invisible (codex, round 7). Every digit run of five or more outside a
# URL must be the expected run id: the round-7 floor was nine digits, so a run
# id shortened to eight left the checked population entirely (codex, round 7).
# Five keeps the template's own small numbers out, and the SHAPE check below is
# what catches a run id shortened below it.
assert_run_references() {  # label file run-id
  local label="$1" file="$2" want_id="$3"
  local want_url bad="" found=0 reference without_urls
  want_url="$(expected_run_url "$want_id")"
  if [[ ! -r "$file" ]]; then
    fail "$label: no payload at $file"
    return
  fi
  while IFS= read -r reference; do
    [[ -n "$reference" ]] || continue
    found=$((found + 1))
    [[ "$reference" == "$want_url" ]] || bad="${bad}url:${reference} "
  done < <(grep -oE 'https?://[^[:space:]]+' "$file" || true)
  without_urls="$(sed -E 's#https?://[^[:space:]]+##g' "$file")"
  while IFS= read -r reference; do
    [[ -n "$reference" ]] || continue
    found=$((found + 1))
    [[ "$reference" == "$want_id" ]] || bad="${bad}id:${reference} "
  done < <(grep -oE '[0-9]{5,}' <<<"$without_urls" || true)
  # A digit run glued to a letter is a corrupted reference, not the run id
  # with a neighbour: `x111111111` contains the correct digits and passed the
  # loop above (codex, round 8). The artifact names legitimately carry the id
  # after a hyphen, so the boundary is alphanumeric-adjacency, not any
  # adjacency.
  while IFS= read -r reference; do
    [[ -n "$reference" ]] || continue
    found=$((found + 1))
    bad="${bad}glued:${reference} "
  done < <(grep -oE '[A-Za-z_][0-9]{5,}|[0-9]{5,}[A-Za-z_]' <<<"$without_urls" || true)
  if ((found > 0)) && [[ -z "$bad" ]]; then
    pass "$label"
  else
    fail "$label: $found reference(s) found, not expected: ${bad:-none}"
  fi
}
# The same payload from two different runs must differ ONLY where the run id
# is substituted. Normalising the expected id out of each and comparing the
# rest catches a run reference this harness has no pattern for: a run id
# shortened to four digits is not the expected id, so it is not normalised, so
# the two payloads differ. No floor, no regex, no population to get wrong.
assert_same_shape() {  # label file-a id-a file-b id-b
  local label="$1" file_a="$2" id_a="$3" file_b="$4" id_b="$5" shape_a shape_b
  if [[ ! -r "$file_a" || ! -r "$file_b" ]]; then
    fail "$label: a payload is missing"
    return
  fi
  # Only a BOUNDED id is normalised. An unanchored substitution turned
  # `x111111111` and `x333333333` into the same `x<RUN>` and the corrupted
  # payloads compared equal (codex, round 8). Bounded, the `x` glues the
  # digits into a token that is not the id, nothing is substituted, and the
  # two payloads differ.
  shape_a="$(sed -E "s/(^|[^[:alnum:]])${id_a}([^[:alnum:]]|$)/\\1<RUN>\\2/g" "$file_a")"
  shape_b="$(sed -E "s/(^|[^[:alnum:]])${id_b}([^[:alnum:]]|$)/\\1<RUN>\\2/g" "$file_b")"
  if [[ "$shape_a" == "$shape_b" ]]; then
    pass "$label"
  else
    fail "$label: the payloads differ beyond the run id: $(diff <(printf '%s\n' "$shape_a") <(printf '%s\n' "$shape_b") | head -6 | tr '\n' '|')"
  fi
}

reset_issues() {  # state [repo] -> an empty issue list for that repository
  local file; file="$(issues_file "$1" "${2-}")"
  mkdir -p "$(dirname "$file")"
  : >"$file"
}
count_comments() {  # state [repo]
  local file; file="$(comments_file "$1" "${2-}")"
  [[ -f "$file" ]] || { echo 0; return; }
  wc -l <"$file" | tr -d ' '
}

# A working tree the lifted step can run in: it needs the filer at its real
# relative path, and, for the language workflow, an artifacts directory.
mkdir -p "$WORK/tree/scripts/ci"
cp scripts/ci/file-fuzz-issue.sh "$WORK/tree/scripts/ci/file-fuzz-issue.sh"

# ==========================================================================
# 0. No fuzz workflow calls gh issue create directly.
#
# Hoisted above everything that can abort, because in round 1 this sat last and
# `set -e` at the lift meant the ADD-form negative control never reached it.
# The verdict is bound to what the loop found rather than announced after it.
# ==========================================================================
# Backslash line continuations are joined before matching, and whitespace is
# collapsed. A contiguous-string grep missed
#   gh \
#     issue create ...
# so a workflow under a non-canonical step name kept a direct create and the
# suite stayed green over it (codex and grok, round 2, independently).
flatten_workflow() {
  sed -e ':a' -e '/\\$/{N;s/\\\n//;ba' -e '}' "$1" | tr -s '[:space:]' ' '
}
# The scan takes a directory so the fixture in 12j drives THIS code rather
# than a second copy of the same grep. Reverting the flattening to a plain
# grep leaves no trace on the shipped tree, so the fixture is the only thing
# that can catch it, and it has to run the real scanner to do so.
list_direct_create_offenders() {
  local dir="$1" workflow
  for workflow in "$dir"/fuzz-*.yml; do
    [[ -f "$workflow" ]] || continue
    if flatten_workflow "$workflow" | grep -q 'gh issue create'; then
      printf '%s\n' "$workflow"
    fi
  done
}
direct_create_offenders=0
while IFS= read -r workflow; do
  [[ -n "$workflow" ]] || continue
  fail "$workflow calls gh issue create directly instead of the filer"
  direct_create_offenders=$((direct_create_offenders + 1))
done < <(list_direct_create_offenders .github/workflows)
if ((direct_create_offenders == 0)); then
  pass "no fuzz workflow calls gh issue create directly"
fi

# ==========================================================================
# 0b. The instrument is itself under test.
#
# Round 1 established that a fake which ignores the flags it is handed cannot
# fail when the subject stops passing one: dropping --json / --jq survived the
# entire suite. So the fake's fidelity to gh 2.90.0 is asserted here rather
# than assumed, and a fake that quietly stops honouring a flag fails now.
# ==========================================================================
state="$WORK/state-instrument"
mkdir -p "$state"
for n in 10 20 30 40 50; do
  seed_issue "$state" "$n" "instrument row $n" "fuzz" "open"
done
seed_issue "$state" 60 "instrument closed row" "fuzz" "closed"
seed_issue "$state" 70 "instrument unlabelled row" "other" "open"

fake_list() {
  ( cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" gh issue list "$@" )
}
TSV_JQ='.[] | "\(.number)\t\(.title)"'

assert_eq "the fake honours --limit" 2 \
  "$(fake_list --state open --label fuzz --limit 2 --json number,title --jq "$TSV_JQ" | wc -l)"
assert_eq "the fake orders newest first, as gh does" "50" \
  "$(fake_list --state open --label fuzz --limit 5 --json number,title --jq "$TSV_JQ" | head -1 | cut -f1)"
assert_eq "the fake honours --state open" 5 \
  "$(fake_list --state open --label fuzz --limit 99 --json number,title --jq "$TSV_JQ" | wc -l)"
assert_eq "the fake honours --state all" 6 \
  "$(fake_list --state all --label fuzz --limit 99 --json number,title --jq "$TSV_JQ" | wc -l)"
assert_eq "the fake honours --label" 1 \
  "$(fake_list --state open --label other --limit 99 --json number,title --jq "$TSV_JQ" | wc -l)"
if fake_list --state open --label fuzz --limit 5 >/dev/null 2>&1; then
  fail "the fake returns TSV without --json, which real gh does not"
else
  pass "the fake refuses a listing that real gh would answer with JSON"
fi
if fake_list --state open --label fuzz --limit 5 --json number,title >/dev/null 2>&1; then
  fail "the fake returns TSV without --jq, which real gh does not"
else
  pass "the fake refuses --json without the TSV --jq expression"
fi
# A projection missing a field renders that field as `null`, which is what
# live 2.90.0 does with the TSV --jq; the round-6 fake refused instead, which
# was the fake doing the harness's job (grok, round 7). The behavioural checks
# are what catch a filer that drops `title`: it matches nothing and creates
# every run.
assert_eq "the fake renders a field the projection omitted as null, as gh does" \
  "50"$'\t'"null" \
  "$(fake_list --state open --label fuzz --limit 1 --json number --jq "$TSV_JQ")"
# `--json=` and `--json ''` are ACCEPTED by live 2.90.0: `[{}]` per issue,
# exit 0, and `null<TAB>null` through the --jq. Refusing them was the round-6
# fake's over-fix, and grok had warned in round 5 that live gh accepts more
# here than assumed.
for empty_projection_spelling in "--json=" "--json"; do
  if [[ "$empty_projection_spelling" == "--json" ]]; then
    empty_projection_out="$(fake_list --state open --label fuzz --limit 1 --json '' --jq "$TSV_JQ" 2>&1)"
  else
    empty_projection_out="$(fake_list --state open --label fuzz --limit 1 --json= --jq "$TSV_JQ" 2>&1)"
  fi
  assert_eq "the fake accepts the empty projection ($empty_projection_spelling), as gh does" \
    "null"$'\t'"null" "$empty_projection_out"
done

# Live gh 2.90.0 answers an unknown field with `Unknown JSON field: "x"`. The
# fake accepted it, and the calls.log check matched it as a prefix, so a filer
# mutated to a live-illegal projection survived at 93/0 while every real lookup
# would fail, the retries would exhaust and it would blind-create every run
# (codex and grok, round 3).
if fake_list --state open --label fuzz --limit 5 --json number,title,notAField --jq "$TSV_JQ" >/dev/null 2>&1; then
  fail "the fake accepts a --json field live gh rejects"
else
  pass "the fake refuses a --json field live gh rejects"
fi
# A field name containing a SPACE is one field to gh and it is unknown. The
# membership test used to be a substring match over the space-joined list, so
# `assignees author` passed it (codex, round 5).
if fake_list --state open --label fuzz --limit 5 --json "number,title,assignees author" --jq "$TSV_JQ" >/dev/null 2>&1; then
  fail "the fake accepts two field names joined by a space as one field"
else
  pass "the fake refuses two field names joined by a space"
fi
# Empty members. Live gh 2.90.0 answers all three with `Unknown JSON field: ""`.
# The sentinel split rejects them today and NOTHING drove them, so inserting a
# skip-empty guard, or reverting to the read -a split that dropped a trailing
# member, both survived at 107/0 (grok, round 5).
for empty_member_projection in "number,title," ",number,title" "number,,title"; do
  if fake_list --state open --label fuzz --limit 5 --json "$empty_member_projection" --jq "$TSV_JQ" >/dev/null 2>&1; then
    fail "the fake accepts the empty-member projection '$empty_member_projection'"
  else
    pass "the fake refuses the empty-member projection '$empty_member_projection'"
  fi
done
# A field gh really has stays legal, so the guard above is not false-red on a
# spelling the filer could move to.
assert_eq "the fake accepts a live-legal extra --json field" 2 \
  "$(fake_list --state open --label fuzz --limit 2 --json number,title,url --jq "$TSV_JQ" | wc -l)"

# --json ACCUMULATES across repetitions, as a pflag StringSlice does. Round 6
# made the fake accumulate and nothing drove two --json flags, so reverting it
# to last-wins survived the whole suite, and with it the filer passing an
# illegal field beside a legal one: the round-6 defect, green again (grok,
# round 7). Both orderings, for the same reason repeated --label has both: a
# last-wins parser agrees with the correct one whenever the LAST value alone
# would decide the outcome.
for json_pair in "notAField number,title" "number,title notAField"; do
  read -r json_first json_second <<<"$json_pair"
  if fake_list --state open --label fuzz --limit 5 --json "$json_first" --json "$json_second" --jq "$TSV_JQ" >/dev/null 2>&1; then
    fail "the fake accepts --json $json_first --json $json_second, which live gh refuses"
  else
    pass "the fake refuses --json $json_first --json $json_second, as live gh does"
  fi
  # The equals spelling is a separate parser arm, and only the space form was
  # driven, so reverting THAT arm to last-wins survived at 179/0 (codex,
  # round 8).
  if fake_list --state open --label fuzz --limit 5 "--json=$json_first" "--json=$json_second" --jq "$TSV_JQ" >/dev/null 2>&1; then
    fail "the fake accepts --json=$json_first --json=$json_second, which live gh refuses"
  else
    pass "the fake refuses --json=$json_first --json=$json_second, as live gh does"
  fi
done
assert_eq "the fake accumulates a projection split across two --json flags" \
  "50"$'\t'"instrument row 50" \
  "$(fake_list --state open --label fuzz --limit 1 --json number --json title --jq "$TSV_JQ")"

# The CSV grammar itself, against what gh 2.90.0 was measured to do. Two
# hand-written readers both accepted characters after a closing quote, so
# `"num"ber,title` decoded to a legal projection while live gh refuses the
# call with `extraneous or missing " in quoted-field`; the filer carrying that
# spelling stayed green at 130/0 and would blind-create every run (codex,
# round 7). Each row is a measured live outcome, not a reading of Go's source.
csv_fidelity_case() {  # projection expected-outcome(accept|refuse) label
  local projection="$1" want="$2" label="$3" got
  if fake_list --state open --label fuzz --limit 1 --json "$projection" --jq "$TSV_JQ" >/dev/null 2>&1; then
    got="accept"
  else
    got="refuse"
  fi
  assert_eq "the fake ${want}s the projection $label, as live gh does" "$want" "$got"
}
csv_fidelity_case '"number","title"'   accept 'with quoted members'
csv_fidelity_case '"num""ber",title'   refuse 'with a doubled quote making an unknown field'
csv_fidelity_case '"num"ber,title'     refuse 'with text after a closing quote'
csv_fidelity_case 'number,"title'      refuse 'with an unterminated quote'
csv_fidelity_case 'num"ber,title'      refuse 'with a bare quote in an unquoted member'
csv_fidelity_case ' number,title'      refuse 'with a leading space in a member'
# One RECORD. pflag calls csv.Reader.Read() once, so a trailing newline is a
# record terminator and not part of the last member, and anything after it
# is never read: `number,title<LF>` and `number,title<LF>notAField` are both
# accepted live. A value that is only a newline is an EOF error live, and the
# fake accepted it (grok and codex, round 8).
csv_fidelity_case $'number,title\n'          accept 'with a trailing newline'
csv_fidelity_case $'number,title\nnotAField' accept 'with a second record after the newline'
csv_fidelity_case $'\n'                      refuse 'that is only a newline'

# gh's repository rule is `[HOST/]OWNER/REPO`. Asking only for a slash let the
# fake accept `cli/` and `cli/cli/extra/path`, both refused live (grok, round
# 7). The fake accepting what gh refuses is the direction that hides defects.
for bad_repo in "noslash" "cli/" "/cli" "cli/cli/extra/path" "cli//cli"; do
  if fake_list --repo "$bad_repo" --state open --label fuzz --limit 1 --json number,title --jq "$TSV_JQ" >/dev/null 2>&1; then
    fail "the fake accepts --repo $bad_repo, which live gh refuses"
  else
    pass "the fake refuses --repo $bad_repo, as live gh does"
  fi
done
for good_repo in "octo/example" "github.com/octo/example"; do
  if fake_list --repo "$good_repo" --state open --label fuzz --limit 1 --json number,title --jq "$TSV_JQ" >/dev/null 2>&1; then
    pass "the fake accepts --repo $good_repo, as live gh does"
  else
    fail "the fake refuses --repo $good_repo, which live gh accepts"
  fi
done
# Repeated --label is AND on live gh, measured: `--label fuzz --label nope`
# returned 0 rows where `--label fuzz` returned 10. The fake took the last one.
# Both orderings, because last-wins and AND agree whenever the LAST label is
# the one that matches nothing. Checking only that ordering let the last-wins
# parser survive re-introduction (codex, round 4). With the missing label
# FIRST, AND still returns nothing and last-wins returns rows.
assert_eq "the fake ANDs repeated --label as live gh does" 0 \
  "$(fake_list --state open --label fuzz --label no-such-label --limit 99 --json number,title --jq "$TSV_JQ" | wc -l)"
assert_eq "the fake ANDs repeated --label whichever one misses" 0 \
  "$(fake_list --state open --label no-such-label --label fuzz --limit 99 --json number,title --jq "$TSV_JQ" | wc -l)"

# ==========================================================================
# 1. Two failing runs of one target produce one issue and one comment.
#
# The set under test is derived from the tree, not listed here: every fuzz
# workflow that carries a filing step. A fifth one is covered the day it lands.
# An empty set would make every assertion below vacuous, so it is refused.
# ==========================================================================
# The set is derived by PARSING the YAML, with the same parser the lift uses.
# It was a raw six-space grep, which is a SECOND oracle: quoting the step name
# as `- name: "File issue on crash"` left the lift working and the grep
# failing, so the workflow silently left the set AND expected_checks shrank to
# match, and the suite stayed green over an untested public-shipping workflow
# (grok, round 1).
derive_filing_workflows() {
  "$PYTHON_BIN" - "$1" <<'DERIVE_SET'
import pathlib, sys, yaml

for path in sorted(pathlib.Path(sys.argv[1]).glob("fuzz-*.yml")):
    try:
        data = yaml.safe_load(path.read_text())
    except yaml.YAMLError as error:
        print(f"FATAL: {path} does not parse: {error}", file=sys.stderr)
        raise SystemExit(1)
    if not isinstance(data, dict):
        continue
    for job in (data.get("jobs") or {}).values():
        if any(s.get("name") == "File issue on crash" for s in job.get("steps", [])):
            print(path.stem)
            break
DERIVE_SET
}

# A process substitution's exit status is lost to mapfile, so a workflow that
# does not parse printed FATAL and the suite still exited 0 with that workflow
# quietly dropped (codex, round 2). Route it through a file and check.
if ! derive_filing_workflows .github/workflows >"$WORK/derived-set" 2>"$WORK/derived-err"; then
  cat "$WORK/derived-err" >&2
  echo "FATAL: the filing-workflow set could not be derived" >&2
  exit 1
fi
mapfile -t filing_workflows <"$WORK/derived-set"

# Cross-check the derived set against an INDEPENDENT signal. Renaming a real
# filing step dropped it from the set AND shrank expected_checks to match, so
# the suite went green at 58 checks with a shipped workflow untested (codex,
# round 2). Any fuzz workflow that references the filer must be in the set;
# a step rename no longer removes it silently, it fails here.
for workflow in .github/workflows/fuzz-*.yml; do
  [[ -f "$workflow" ]] || continue
  grep -q 'scripts/ci/file-fuzz-issue.sh' "$workflow" || continue
  stem="$(basename "$workflow" .yml)"
  found="false"
  for known in "${filing_workflows[@]}"; do
    [[ "$known" == "$stem" ]] && found="true"
  done
  if [[ "$found" != "true" ]]; then
    echo "FATAL: $workflow calls the filer but is not in the derived set." >&2
    echo "FATAL: its filing step is probably named something other than" >&2
    echo "FATAL: 'File issue on crash', which would leave it untested." >&2
    exit 1
  fi
done
# The derivation must be a PARSE, not a text match. A raw six-space grep left
# a workflow whose step name was quoted out of the set, and expected_checks
# shrank with it, so the suite stayed green over an untested public-shipping
# workflow (grok, round 1). This drives the shipped derivation against a
# fixture that spells the step name every legal way.
fixture_dir="$WORK/derivation-fixture"
mkdir -p "$fixture_dir"
cat >"$fixture_dir/fuzz-quoted.yml" <<'FIXTURE'
name: fuzz-quoted
on: workflow_dispatch
jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - name: "File issue on crash"
        run: scripts/ci/file-fuzz-issue.sh --title t --body-file b --comment-file c
FIXTURE
cat >"$fixture_dir/fuzz-plain.yml" <<'FIXTURE'
name: fuzz-plain
on: workflow_dispatch
jobs:
  fuzz:
    runs-on: ubuntu-latest
    steps:
      - name: File issue on crash
        run: scripts/ci/file-fuzz-issue.sh --title t --body-file b --comment-file c
FIXTURE
cat >"$fixture_dir/fuzz-nofiling.yml" <<'FIXTURE'
name: fuzz-nofiling
on: workflow_dispatch
jobs:
  cleanup:
    runs-on: ubuntu-latest
    steps:
      - name: Something else
        run: "true"
FIXTURE
assert_eq "the workflow set is derived by parsing, not by matching text" \
  "fuzz-plain fuzz-quoted" "$(derive_filing_workflows "$fixture_dir" | sort | tr '\n' ' ' | sed 's/ $//')"

if ((${#filing_workflows[@]} == 0)); then
  echo "FATAL: no fuzz workflow carries a filing step, so this harness would assert nothing" >&2
  exit 1
fi
printf 'note\tfiling workflows under test: %s\n' "${filing_workflows[*]}"
expected_checks=$((${#filing_workflows[@]} * CHECKS_PER_WORKFLOW + FIXED_CHECKS))
# The two derived per-workflow terms are added where those sets are built.

for workflow in "${filing_workflows[@]}"; do
  script="$WORK/$workflow.sh"
  extract_step ".github/workflows/$workflow.yml" "$script" "$WORK/$workflow.env"
  bash -n "$script" || { fail "$workflow: lifted run script is not valid bash"; continue; }

  # Each substituted env name is pinned to the DECLARATION it must carry, not
  # to the shape of one. Asking only whether the value contained `${{` and
  # `}}` accepted `${{ '"'"'svelte_plugin'"'"' }}`, which is a constant wearing
  # expression syntax, and `${{ matrix.target && '"'"'svelte_plugin'"'"' }}`,
  # which mentions the context and still collapses every target onto one issue
  # (codex, round 6). GH_TOKEN is here because a literal there is a hardcoded
  # credential. Whitespace inside the braces is not significant to Actions and
  # is not significant here either.
  env_literal=""
  while IFS=$'\t' read -r env_name env_value; do
    [[ -n "$env_name" ]] || continue
    # shellcheck disable=SC2016  # literal GitHub expression syntax
    case "$env_name" in
      TARGET)    env_want='${{matrix.target}}' ;;
      CRATE_DIR) env_want='${{matrix.crate}}' ;;
      RUN_ID)    env_want='${{github.run_id}}' ;;
      RUN_URL)   env_want='${{github.server_url}}/${{github.repository}}/actions/runs/${{github.run_id}}' ;;
      GH_TOKEN)  env_want='${{secrets.GITHUB_TOKEN}}' ;;
      *)         continue ;;
    esac
    [[ "${env_value//[[:space:]]/}" == "$env_want" ]] \
      || env_literal="${env_literal}${env_name}=${env_value} "
  done <"$WORK/$workflow.env"
  if [[ -z "$env_literal" ]]; then
    pass "$workflow: every substituted env value is the declaration it must be"
  else
    fail "$workflow: these env values are not the declaration they must be: $env_literal"
  fi
  # The filing step runs on FAILURE. Its condition may narrow that further,
  # as the selection-gated workflows do, but it must begin with `failure()`,
  # or the step files an issue on a green run (cursor, round 8).
  step_if="$(cat "$script.if")"
  if [[ "${step_if//[[:space:]]/}" == 'failure()' || "${step_if//[[:space:]]/}" == 'failure()&&'* ]]; then
    pass "$workflow: the filing step is conditioned on failure()"
  else
    fail "$workflow: the filing step's condition is '${step_if}', not failure()"
  fi

  state="$WORK/state-$workflow"
  mkdir -p "$state"

  # The language workflow reads a real artifact name off disk.
  rm -rf "$WORK/tree/sqry-core"
  mkdir -p "$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin"
  : >"$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin/crash-deadbeef"

  run_step "$script" "$state" 111111111 svelte_plugin sqry-core/fuzz >"$state/out1" 2>"$state/err1" \
    || { fail "$workflow: first run failed: $(cat "$state/err1")"; continue; }
  run_step "$script" "$state" 222222222 svelte_plugin sqry-core/fuzz >"$state/out2" 2>"$state/err2" \
    || { fail "$workflow: second run failed: $(cat "$state/err2")"; continue; }

  assert_eq "$workflow: two failing runs leave one open issue" 1 "$(count_issues "$state")"
  assert_eq "$workflow: the second run comments instead of filing" 1 "$(count_comments "$state")"
  assert_eq "$workflow: first run reports a create" \
    "created" "$(awk '{print $1}' "$state/out1")"
  assert_eq "$workflow: second run reports a comment" \
    "commented" "$(awk '{print $1}' "$state/out2")"

  title="$(cut -f2 "$(issues_file "$state")")"
  if [[ "$title" == *111111111* || "$title" == *222222222* ]]; then
    fail "$workflow: the issue title carries a run id: $title"
  else
    pass "$workflow: the issue title is a stable key ($title)"
  fi

  # EVERY run reference the payload carries, checked against an expectation
  # built here rather than read out of the payload. `grep -q 111111111` asked
  # only whether the right run appeared SOMEWHERE, so appending a digit to
  # RUN_ID left the original id visible inside the longer one and the check
  # stayed green; the same held for RUN_URL, where a correct run id elsewhere
  # in the body masked a wrong URL (codex, round 6).
  number="$(cut -f1 "$(issues_file "$state")")"
  assert_run_references "$workflow: the body names the first run and no other" \
    "$(gh_fake_bucket "$state")/body-$number.md" 111111111
  assert_run_references "$workflow: the comment names the later run and no other" \
    "$(gh_fake_bucket "$state")/comment-$number.md" 222222222

  # The same two runs again, in a fresh state, with different run ids. The
  # payloads must match the first pair once the run id is normalised out.
  state_b="$WORK/state-$workflow-shape"
  mkdir -p "$state_b"
  run_step "$script" "$state_b" 333333333 svelte_plugin sqry-core/fuzz >/dev/null 2>&1 || true
  run_step "$script" "$state_b" 444444444 svelte_plugin sqry-core/fuzz >/dev/null 2>&1 || true
  number_b="$(cut -f1 "$(issues_file "$state_b")" 2>/dev/null | head -1)"
  assert_same_shape "$workflow: two bodies differ only in the run id" \
    "$(gh_fake_bucket "$state")/body-$number.md" 111111111 \
    "$(gh_fake_bucket "$state_b")/body-${number_b:-none}.md" 333333333
  assert_same_shape "$workflow: two comments differ only in the run id" \
    "$(gh_fake_bucket "$state")/comment-$number.md" 222222222 \
    "$(gh_fake_bucket "$state_b")/comment-${number_b:-none}.md" 444444444

  # The labels the create carried, as a COMPLETE set. The lookup narrows to
  # `fuzz`, so losing that one fails seven checks; losing `bug` was invisible,
  # because nothing read the labels back at all (all three reviewers, round 7).
  # The workflows pass `--label "bug,fuzz"` and the filer defaults to the same,
  # so both sites are pinned to the one policy.
  assert_eq "$workflow: the issue is created with exactly the labels bug and fuzz" \
    "bug fuzz" \
    "$(gh_calls "$state" optmembers 'issue create' 0 --label | sort | tr '\n' ' ' | sed 's/ $//')"
done

# --------------------------------------------------------------------------
# The step's environment is REPLACED, not added to.
#
# `env NAME=value cmd` leaves the caller's environment in place, so an ambient
# TARGET reached the child and this harness's claim to supply exactly the
# declared environment was false. A workflow whose run body reads a variable it
# never declared would have passed here and died under `set -u` in production
# (codex, round 6). The probe runs through the same construction path the
# lifted steps use, so it cannot drift from them.
# --------------------------------------------------------------------------
env_probe="$WORK/env-probe.sh"
printf '%s\n' 'printf "%s|%s\n" "${TARGET}" "${AMBIENT_ONLY:-unset}"' >"$env_probe"
# shellcheck disable=SC2016  # literal GitHub expression syntax
printf 'TARGET\t${{ matrix.target }}\n' >"$WORK/env-probe.env"
mkdir -p "$WORK/state-env-probe"
env_probe_out="$(TARGET=ambient-leak AMBIENT_ONLY=leaked \
  run_step "$env_probe" "$WORK/state-env-probe" 999999999 probe-target "" 2>&1)"
assert_eq "the lifted step sees the declared environment and nothing ambient" \
  "probe-target|unset" "$env_probe_out"

# ==========================================================================
# The scenarios below drive the filer directly rather than through a lifted
# workflow. They are properties of the filer, and the sanitized public mirror
# ships one fuzz workflow where the internal tree ships four, so binding them
# to a particular workflow would quietly drop them there.
# ==========================================================================
: >"$WORK/body.md"
: >"$WORK/comment.md"

# Extra NAME=VALUE arguments after the title are passed to `env`; an expanded
# "$@" cannot act as an assignment prefix, so it has to be a real command.
file_issue() {
  local state="$1" title="$2"
  shift 2
  (
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" FUZZ_ISSUE_SETTLE_SECONDS=0 FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake "$@" \
      ./scripts/ci/file-fuzz-issue.sh \
        --title "$title" \
        --body-file "$WORK/body.md" \
        --comment-file "$WORK/comment.md"
  )
}

# --------------------------------------------------------------------------
# 2. Two different targets keep their own issue.
# --------------------------------------------------------------------------
state="$WORK/state-distinct"
mkdir -p "$state"
file_issue "$state" "language plugin fuzz crash: svelte_plugin" >/dev/null
file_issue "$state" "language plugin fuzz crash: sql_plugin" >/dev/null
assert_eq "distinct targets are not collapsed into one issue" 2 "$(count_issues "$state")"

# --------------------------------------------------------------------------
# 3. Two different finding kinds on one target keep their own issue.
# --------------------------------------------------------------------------
state="$WORK/state-kinds"
mkdir -p "$state"
file_issue "$state" "language plugin fuzz crash: svelte_plugin" >/dev/null
file_issue "$state" "language plugin fuzz memory leak: svelte_plugin" >/dev/null
assert_eq "a leak and a crash on one target are separate issues" 2 "$(count_issues "$state")"

# --------------------------------------------------------------------------
# 4. A recurrence after the issue was closed is filed again. The lookup asks
#    for open issues only, so a fixed-and-closed finding that comes back gets
#    a fresh report instead of a comment nobody is watching.
# --------------------------------------------------------------------------
state="$WORK/state-closed"
mkdir -p "$state"
file_issue "$state" "fuzz-critical crash: lsp_protocol_parse" >/dev/null
# CLOSE it, leaving the row in the listing. Erasing the whole file instead made
# this scenario pass for a filer that ignored --state entirely (grok, round 1).
sed -i 's/\topen$/\tclosed/' "$(issues_file "$state")"
out="$(file_issue "$state" "fuzz-critical crash: lsp_protocol_parse")"
assert_eq "a recurrence after a close is filed again" "created" "$(awk '{print $1}' <<<"$out")"
assert_eq "the closed issue is not commented on" 0 "$(count_comments "$state")"

# --------------------------------------------------------------------------
# 5. A lookup that cannot be performed reports the finding rather than
#    dropping it, and says in the issue that it was filed blind.
# --------------------------------------------------------------------------
state="$WORK/state-lookup-fails"
mkdir -p "$state"
out="$(file_issue "$state" "fuzz-critical crash: lsp_protocol_parse" GH_FAKE_LIST_FAILS=1 2>/dev/null)"
assert_eq "a failed lookup still files the finding" "created" "$(awk '{print $1}' <<<"$out")"
number="$(cut -f1 "$(issues_file "$state")")"
if grep -q "Deduplication was skipped" "$(gh_fake_bucket "$state")/body-$number.md"; then
  pass "an issue filed blind says so in its body"
else
  fail "an issue filed blind does not disclose it"
fi

# --------------------------------------------------------------------------
# 6. The filer refuses a title that still carries a run id, which is the exact
#    shape that produced 26 issues for 14 findings.
# --------------------------------------------------------------------------
state="$WORK/state-guard"
mkdir -p "$state"
if file_issue "$state" \
  "language plugin fuzz crash: svelte_plugin (run 33737060861)" >/dev/null 2>&1; then
  fail "the filer accepted a title carrying a run id"
else
  pass "the filer refuses a title carrying a run id"
fi

# --------------------------------------------------------------------------
# 7. A listing that has not caught up yet does not authorize a second issue.
#    Measured against the live API on 2026-09-04: three back-to-back runs of
#    one title made three issues, because each read a listing from before the
#    previous create. GH_FAKE_LIST_LAG=1 reproduces exactly that.
# --------------------------------------------------------------------------
state="$WORK/state-lag"
mkdir -p "$state"
GH_FAKE_LIST_LAG=1 file_issue "$state" "fuzz-critical crash: lsp_protocol_parse" >/dev/null
out="$(GH_FAKE_LIST_LAG=1 file_issue "$state" "fuzz-critical crash: lsp_protocol_parse")"
assert_eq "a stale listing does not authorize a second issue" 1 "$(count_issues "$state")"
assert_eq "the run behind a stale listing comments" "commented" "$(awk '{print $1}' <<<"$out")"

# --------------------------------------------------------------------------
# 8. The settle delay is real, measured by the clock rather than by reading
#    the source. A `sed` against the default assignment only caught a changed
#    DEFAULT; gutting the `sleep` while leaving the assignment at ten survived
#    (grok, round 1). This runs the filer with a two-second delay and times it.
# --------------------------------------------------------------------------
state="$WORK/state-settle"
mkdir -p "$state"

# Timed TWICE, with different requested delays, and the elapsed time must track
# the request. Round 2 asked for one delay and asserted `elapsed >= 2`, so a
# hardcoded `sleep 2` satisfied it while being shorter than the measured
# five-second listing lag; production never passes the flag and would have
# waited two seconds instead of ten (grok, round 2). No single constant can
# satisfy both bounds below.
time_settle() {  # seconds -> elapsed
  local want="$1" started
  started="$(date +%s)"
  (
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
      ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: timed-$want" \
        --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" \
        --settle-seconds "$want"
  ) >/dev/null 2>&1 || true
  echo $(( $(date +%s) - started ))
}
short_elapsed="$(time_settle 1)"
long_elapsed="$(time_settle 4)"
if ((short_elapsed >= 1 && short_elapsed < 4)); then
  pass "a one-second settle waits about one second (${short_elapsed}s)"
else
  fail "a one-second settle took ${short_elapsed}s, so the wait is not the requested one"
fi
if ((long_elapsed >= 4)); then
  pass "a four-second settle waits at least four seconds (${long_elapsed}s)"
else
  fail "a four-second settle took ${long_elapsed}s, so the wait is not the requested one"
fi

# The default must still cover the measured five-second listing lag. Reading it
# rather than paying it, because a run at the real default would add ten
# seconds to every create in this suite.
# shellcheck disable=SC2016  # the sed script is literal, not a shell expansion
default_settle="$(
  sed -n 's/^settle_seconds="\${FUZZ_ISSUE_SETTLE_SECONDS:-\([0-9]*\)}"$/\1/p' \
    scripts/ci/file-fuzz-issue.sh
)"
if [[ -n "$default_settle" ]] && ((default_settle > 5)); then
  pass "the default settle exceeds the measured listing lag (${default_settle}s)"
else
  fail "the default settle does not exceed the measured five-second listing lag"
fi

# --------------------------------------------------------------------------
# 8b. The confirming lookup beats a WALL-CLOCK lag, not just a call counter.
#
# This is the behaviour the whole settle exists for, driven the way the live
# incident happened: an issue that is invisible for two seconds after it is
# created. With a settle longer than the lag the second run must comment. With
# no settle it must duplicate, which is what proves the delay is doing the work
# rather than the call counter.
# --------------------------------------------------------------------------
state="$WORK/state-wallclock"
mkdir -p "$state"
wallclock_run() {  # settle-seconds
  (
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" GH_FAKE_LIST_LAG_SECONDS=2 \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
      ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: wallclock" \
        --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" \
        --settle-seconds "$1"
  ) 2>/dev/null
}
wallclock_run 0 >/dev/null
second="$(wallclock_run 4)"
assert_eq "a settle longer than the listing lag comments instead of duplicating" \
  "commented" "$(awk '{print $1}' <<<"$second")"
assert_eq "the wall-clock lag left one issue, not two" 1 "$(count_issues "$state")"

state="$WORK/state-wallclock-nosettle"
mkdir -p "$state"
wallclock_nosettle() {
  (
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" GH_FAKE_LIST_LAG_SECONDS=120 \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
      ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: nosettle" \
        --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" \
        --settle-seconds 0
  ) 2>/dev/null
}
wallclock_nosettle >/dev/null
wallclock_nosettle >/dev/null
assert_eq "without a settle the same wall-clock lag DOES duplicate" 2 "$(count_issues "$state")"

# --------------------------------------------------------------------------
# 9. The lookup asks for the TSV projection. Dropping --json / --jq survived
#    the whole suite in round 1, and against live gh that returns colorized
#    JSON the read loop cannot parse, so every run would create (grok).
#    The fake now refuses any other flag combination, which is what makes the
#    mutation fail here rather than in production.
# --------------------------------------------------------------------------
state="$WORK/state-flags"
mkdir -p "$state"
file_issue "$state" "fuzz-critical crash: flags" >/dev/null
# The projection is read back the way gh builds it: every --json occurrence
# accumulated and CSV-decoded, then compared MEMBER BY MEMBER. Reading one
# occurrence out of a space-joined line could not see a second --json at all,
# so `--json notAField --json number,title` satisfied this check while live gh
# rejects the whole call, the three retries exhaust and the filer blind-creates
# on every run (codex and grok, round 6). Before that a substring match had
# accepted a prefix (round 3) and a space-joined member (round 5): the same
# validator, three times, each time because the comparison was looser than the
# thing it claimed to check.
# A legal EXTRA field stays green on purpose, because the fake refuses an
# illegal one independently; these two guards are meant to be separable.
mapfile -t lookup_json_fields < <(gh_calls "$state" optmembers 'issue list' 0 --json)
lookup_json_missing=""
for required_field in number title; do
  lookup_json_has=""
  for lookup_json_field in ${lookup_json_fields[@]+"${lookup_json_fields[@]}"}; do
    [[ "$lookup_json_field" == "$required_field" ]] && { lookup_json_has=1; break; }
  done
  [[ -n "$lookup_json_has" ]] || lookup_json_missing="${lookup_json_missing}${required_field} "
done
if ((${#lookup_json_fields[@]} > 0)) && [[ -z "$lookup_json_missing" ]]; then
  pass "the lookup asks for the number,title JSON projection"
else
  fail "the lookup projection is '${lookup_json_fields[*]-}', missing '${lookup_json_missing}'"
fi
# The whole --jq program, not the presence of the flag. Presence was satisfied
# by any value at all.
assert_eq "the lookup asks gh to render the TSV" \
  '.[] | "\(.number)\t\(.title)"' "$(gh_calls "$state" opt 'issue list' 0 --jq)"
# The filer's DEFAULT labels, as a complete set, on the direct path. The
# per-workflow check pins what the workflows pass; this pins what the filer
# does when nothing is passed.
assert_eq "the filer's default create labels are exactly bug and fuzz" \
  "bug fuzz" \
  "$(gh_calls "$state" optmembers 'issue create' 0 --label | sort | tr '\n' ' ' | sed 's/ $//')"

# Emptying repo_args entirely survived the suite, because nothing checked that
# a --repo the caller gave is forwarded (codex, round 3). A filer that drops it
# reads and writes whatever repository gh happens to resolve, which on a
# self-hosted runner is not necessarily the one that crashed.
state="$WORK/state-repo-forward"
mkdir -p "$state"
repo_forward_run() {
  (
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" FUZZ_ISSUE_SETTLE_SECONDS=0 \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
      ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: repo-forward" \
        --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" \
        --repo octo/example
  ) >/dev/null 2>&1
}
# TWICE, so the create path and the comment path both run. A single run in an
# empty state only ever creates.
repo_forward_run
repo_forward_run
# Per call KIND, not "somewhere in the log". A grep over the whole file was
# satisfied by the lookup alone, so dropping "${repo_args[@]}" from the create
# invocation, or from the comment invocation, each survived at 103/0 (codex and
# grok, round 4, independently). Every call the filer makes carries the
# repository or the filer is writing to whichever one gh happens to resolve.
# The EFFECTIVE value is compared whole, after gh's own last-wins rule for a
# plain string flag. Splitting a space-joined log line on whitespace could not
# tell one argument from two, so a single argument spelled
# `octo/wrong --repo octo/example` read as a correct forward, and so did
# `--repo octo/example --repo octo/wrong`, where gh sends the request to
# octo/wrong (codex, round 6). Neither is a spelling the filer would reach by
# accident; both are spellings the check claimed to exclude.
repo_forward_missing=""
for repo_forward_sub in "issue list" "issue create" "issue comment"; do
  while IFS= read -r repo_forward_effective; do
    [[ "$repo_forward_effective" == "octo/example" ]] \
      || repo_forward_missing="${repo_forward_missing}${repo_forward_sub}(${repo_forward_effective}) "
  done < <(gh_calls "$state" repo "$repo_forward_sub")
done
repo_forward_kinds="$(gh_calls "$state" subs)"
if [[ -z "$repo_forward_missing" ]]; then
  pass "every gh call carries the caller's --repo ($repo_forward_kinds)"
else
  fail "these gh calls resolved against another repository: $repo_forward_missing"
fi
# And the state proves it, not just the argv: the repository the caller named
# holds the issue and the one gh would have resolved by itself holds nothing.
# This is the check that does not depend on reading the arguments correctly.
assert_eq "the forwarded run wrote nothing to the default repository" \
  0 "$(count_issues "$state")"
assert_eq "the forwarded run's issue is in the repository the caller named" \
  1 "$(count_issues "$state" "octo/example")"
assert_eq "the forwarded run's comment is in the repository the caller named" \
  1 "$(count_comments "$state" "octo/example")"
# The kinds themselves are asserted, so the check above cannot pass by never
# reaching the write paths at all.
if [[ "$repo_forward_kinds" == "issue comment,issue create,issue list," ]]; then
  pass "the repo-forwarding run exercised list, create and comment"
else
  fail "the repo-forwarding run exercised only: $repo_forward_kinds"
fi
# By VALUE. `grep -- '--state open'` was satisfied by the substring, so
# `--state opened` (which live gh refuses outright) matched, and `--limit`
# matched without any value at all.
assert_eq "the lookup asks for open issues only" \
  "open" "$(gh_calls "$state" opt 'issue list' 0 --state)"
lookup_limit="$(gh_calls "$state" opt 'issue list' 0 --limit)"
if [[ "$lookup_limit" =~ ^[1-9][0-9]*$ ]] && ((lookup_limit > 30)); then
  pass "the lookup sets an explicit limit above gh's default ($lookup_limit)"
else
  fail "the lookup limit is '$lookup_limit', not a number above gh's default of 30"
fi

# The label narrowing is load bearing, so it is checked by behaviour and not by
# the presence of a flag. An open issue with the exact title but WITHOUT the
# label is not this system's issue and must not absorb a finding.
#
# The trade-off, recorded because it is a real one: if label application ever
# failed at create time, that issue would be invisible to every later lookup
# and the next run would duplicate it once. Listing every open issue instead
# would trade that for matching an unrelated human-filed title and for far more
# exposure to the --limit truncation above (codex, round 1).
state="$WORK/state-unlabelled"
mkdir -p "$state"
seed_issue "$state" 500 "fuzz-critical crash: unlabelled" "wontfix" "open"
out="$(file_issue "$state" "fuzz-critical crash: unlabelled")"
assert_eq "an open issue without the label does not absorb a finding" \
  "created" "$(awk '{print $1}' <<<"$out")"

# --------------------------------------------------------------------------
# 10. Oldest wins. gh orders by creation descending, so the loop must keep the
#     LAST match it sees. Breaking on the first match (newest wins) survived
#     round 1 because no scenario ever had two open issues sharing a title.
# --------------------------------------------------------------------------
state="$WORK/state-oldest"
mkdir -p "$state"
seed_issue "$state" 200 "fuzz-critical crash: twinned" "fuzz" "open"
seed_issue "$state" 400 "fuzz-critical crash: twinned" "fuzz" "open"
file_issue "$state" "fuzz-critical crash: twinned" >/dev/null
assert_eq "a recurrence comments on the OLDEST of two duplicates" \
  "200" "$(head -1 "$(comments_file "$state")")"

# --------------------------------------------------------------------------
# 11. A transient lookup failure is retried rather than treated as no match.
#     Cutting the retry loop to one attempt survived round 1.
# --------------------------------------------------------------------------
state="$WORK/state-retry"
mkdir -p "$state"
seed_issue "$state" 300 "fuzz-critical crash: flaky" "fuzz" "open"
out="$(
  cd "$WORK/tree"
  env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" GH_FAKE_LIST_FAIL_TIMES=2 \
    FUZZ_ISSUE_SETTLE_SECONDS=0 FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
    ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: flaky" \
      --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" 2>/dev/null
)"
assert_eq "two failed lookups are retried, not read as no match" \
  "commented" "$(awk '{print $1}' <<<"$out")"
assert_eq "the retried run files nothing" 1 "$(count_issues "$state")"

# --------------------------------------------------------------------------
# 12. A listing truncated at --limit must not authorize a create. gh orders
#     newest first and stops at the limit, so a full page has dropped exactly
#     the oldest issues this lookup is looking for (codex, round 1).
# --------------------------------------------------------------------------
state="$WORK/state-truncated"
mkdir -p "$state"
# FIVE rows against a limit of three, so the page is genuinely cut. Round 2
# seeded exactly three against a limit of three, which is at the limit and not
# past it, so the check passed without ever constructing a truncated result
# (codex, round 2).
for n in 601 602 603 604 605; do
  seed_issue "$state" "$n" "fuzz-critical crash: filler-$n" "fuzz" "open"
done
# The status is written to a file rather than a variable: an assignment made
# inside a command substitution is lost to the parent shell.
truncated_status_file="$WORK/truncated-status"
truncated_run() {  # state title -> prints output, writes the status to a file
  local out
  set +e
  out="$(
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$1" FUZZ_ISSUE_SETTLE_SECONDS=0 \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
      ./scripts/ci/file-fuzz-issue.sh --title "$2" \
        --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" \
        --limit 3 2>&1
  )"
  printf '%s' "$?" >"$truncated_status_file"
  set -e
  printf '%s' "$out"
}
out="$(truncated_run "$state" "fuzz-critical crash: unseen")"
assert_eq "a cut listing refuses rather than creating" 3 "$(cat "$truncated_status_file")"
assert_eq "a cut listing files nothing" 5 "$(count_issues "$state")"
if grep -q "more than 3 open" <<<"$out"; then
  pass "the truncation refusal says why"
else
  fail "the truncation refusal does not explain itself: $out"
fi

# --------------------------------------------------------------------------
# 12a. A cut page that DOES contain a match still refuses.
#
# This is round 2's blocker. The guard only fired when the page held no match,
# so a page holding a NEWER duplicate while the OLDEST was cut away commented
# on the wrong issue and broke the oldest-wins rule the filer claims. Codex
# built exactly this and got '400' where '100' was owed.
# --------------------------------------------------------------------------
state="$WORK/state-paged-twin"
mkdir -p "$state"
seed_issue "$state" 100 "fuzz-critical crash: paged-twin" "fuzz" "open"
seed_issue "$state" 200 "fuzz-critical crash: filler-200" "fuzz" "open"
seed_issue "$state" 300 "fuzz-critical crash: filler-300" "fuzz" "open"
seed_issue "$state" 400 "fuzz-critical crash: paged-twin" "fuzz" "open"
out="$(truncated_run "$state" "fuzz-critical crash: paged-twin")"
assert_eq "a cut page holding a newer duplicate refuses too" 3 "$(cat "$truncated_status_file")"
assert_eq "it does not comment on the newer duplicate" 0 "$(count_comments "$state")"

# --------------------------------------------------------------------------
# 12a-1. A page of EXACTLY the limit is not cut, and must not be refused.
#
# Every truncation case above seeds five rows against a limit of three, which
# both `rows > limit` and `rows >= limit` refuse. So flipping the comparison
# survived the whole suite while, against the fake, the shipped filer creates
# and comments at exactly the limit and the mutant refuses with exit 3 (codex
# and grok, round 3, independently). The boundary is the check.
# --------------------------------------------------------------------------
state="$WORK/state-exact-limit"
mkdir -p "$state"
for n in 701 702 703; do
  seed_issue "$state" "$n" "fuzz-critical crash: exact-$n" "fuzz" "open"
done
truncated_run "$state" "fuzz-critical crash: unseen-at-limit" >/dev/null
assert_eq "a page of exactly the limit is not cut, so an unseen title creates" \
  0 "$(cat "$truncated_status_file")"
assert_eq "the exact-limit create landed" 4 "$(count_issues "$state")"

state="$WORK/state-exact-limit-match"
mkdir -p "$state"
seed_issue "$state" 711 "fuzz-critical crash: exact-match" "fuzz" "open"
seed_issue "$state" 712 "fuzz-critical crash: exact-712" "fuzz" "open"
seed_issue "$state" 713 "fuzz-critical crash: exact-713" "fuzz" "open"
truncated_run "$state" "fuzz-critical crash: exact-match" >/dev/null
assert_eq "a page of exactly the limit comments on a match rather than refusing" \
  0 "$(cat "$truncated_status_file")"
assert_eq "the exact-limit match commented on the open issue" 1 "$(count_comments "$state")"

# --------------------------------------------------------------------------
# 12a-2. Truncation that first appears at the CONFIRMING lookup.
#
# There are two exit-3 sites, one per lookup, and only the first was covered:
# replacing the post-confirmation one with `:` survived at 93/0 while the
# mutant created a duplicate (codex, round 3). A call-counting lag empties the
# first lookup, so the cut page is seen only the second time.
# --------------------------------------------------------------------------
state="$WORK/state-confirm-truncated"
mkdir -p "$state"
for n in 721 722 723 724; do
  seed_issue "$state" "$n" "fuzz-critical crash: confirm-$n" "fuzz" "open"
done
confirm_truncated_run() {
  local out
  set +e
  out="$(
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" FUZZ_ISSUE_SETTLE_SECONDS=0 \
      FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_FAKE_LIST_LAG=1 GH_TOKEN=fake \
      ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: confirm-unseen" \
        --body-file "$WORK/body.md" --comment-file "$WORK/comment.md" \
        --limit 3 2>&1
  )"
  printf '%s' "$?" >"$truncated_status_file"
  set -e
  printf '%s' "$out"
}
confirm_truncated_run >/dev/null
assert_eq "truncation appearing only at the confirming lookup still refuses" \
  3 "$(cat "$truncated_status_file")"
assert_eq "the confirming-lookup refusal files nothing" 4 "$(count_issues "$state")"
# The lag is what makes this case pin the SECOND exit site: without it the
# first lookup already sees the cut page and refuses there, so the fixture
# would pass while proving nothing about the confirming one. Setting the lag to
# zero left it green (codex, round 4). Two lookups is the signal that the first
# came back empty and the refusal came from the second.
assert_eq "the confirming-lookup case really did look twice" 2 \
  "$(gh_calls "$state" count 'issue list')"

# The same two duplicates WITHIN a page that is not cut must still pick the
# oldest, so the refusal above is truncation and not a blanket refusal of
# duplicates.
state="$WORK/state-uncut-twin"
mkdir -p "$state"
seed_issue "$state" 100 "fuzz-critical crash: uncut-twin" "fuzz" "open"
seed_issue "$state" 400 "fuzz-critical crash: uncut-twin" "fuzz" "open"
file_issue "$state" "fuzz-critical crash: uncut-twin" >/dev/null
assert_eq "an uncut page with two duplicates comments on the oldest" \
  "100" "$(head -1 "$(comments_file "$state")")"

# --------------------------------------------------------------------------
# 12b. Title matching is exact, not case-folded, and the delimiter and run-id
#      guards are load bearing. Each of these survived round 2's own battery
#      until it got a check.
# --------------------------------------------------------------------------
state="$WORK/state-case"
mkdir -p "$state"
seed_issue "$state" 800 "fuzz-critical crash: CaseSensitive" "fuzz" "open"
out="$(file_issue "$state" "fuzz-critical crash: casesensitive")"
assert_eq "a differently-cased title is not treated as the same finding" \
  "created" "$(awk '{print $1}' <<<"$out")"
# An open issue whose title CONTAINS the finding's title is not that finding,
# and neither is one the finding's title contains. Nothing drove either, so
# loosening the filer's comparison to `*"$title"*` survived at 130/0 (codex,
# round 7): a substring match merges distinct findings, which is the original
# defect in the other direction.
state="$WORK/state-superstring"
mkdir -p "$state"
seed_issue "$state" 810 "prefix fuzz-critical crash: super suffix" "fuzz" "open"
out="$(file_issue "$state" "fuzz-critical crash: super")"
assert_eq "an open title that merely contains the finding's title is not matched" \
  "created" "$(awk '{print $1}' <<<"$out")"
state="$WORK/state-substring"
mkdir -p "$state"
seed_issue "$state" 820 "fuzz-critical crash: super" "fuzz" "open"
out="$(file_issue "$state" "fuzz-critical crash: sup")"
assert_eq "an open title the finding's title is a prefix of is not matched" \
  "created" "$(awk '{print $1}' <<<"$out")"
# The two above kill a contains-match and a prefix-match. They are silent on
# the neighbouring axis: an open title that ENDS with the finding's title is
# not that finding either, and `*"$title"` survived at 179/0 (grok, codex and
# cursor, round 8, independently). Same for the unquoted `$title`, which
# turns the title into a glob: no fixture title carried a glob character, so
# nothing changed (codex, round 8).
state="$WORK/state-suffix"
mkdir -p "$state"
seed_issue "$state" 830 "prefix fuzz-critical crash: super" "fuzz" "open"
out="$(file_issue "$state" "fuzz-critical crash: super")"
assert_eq "an open title that ends with the finding's title is not matched" \
  "created" "$(awk '{print $1}' <<<"$out")"
state="$WORK/state-glob"
mkdir -p "$state"
seed_issue "$state" 840 "fuzz-critical crash: abc" "fuzz" "open"
out="$(file_issue "$state" "fuzz-critical crash: a*")"
assert_eq "a finding title is compared as text, not as a glob" \
  "created" "$(awk '{print $1}' <<<"$out")"

state="$WORK/state-delimiter"
mkdir -p "$state"
reset_issues "$state"
set +e
file_issue "$state" "fuzz-critical crash: with$(printf '\t')tab" >/dev/null 2>&1
tab_status=$?
set -e
assert_eq "a title containing a tab is refused before any lookup" 2 "$tab_status"
assert_eq "the refused title filed nothing" 0 "$(count_issues "$state")"

# The run-id guard matched any nine consecutive digits until round 2. That
# refused a legitimate stable title whose target carried nine digits, which
# would take the workflow down for a real crash rather than duplicate an issue.
state="$WORK/state-digits"
mkdir -p "$state"
out="$(file_issue "$state" "language plugin fuzz crash: target_123456789")"
assert_eq "a stable title whose target carries nine digits is accepted" \
  "created" "$(awk '{print $1}' <<<"$out")"

# --------------------------------------------------------------------------
# 12c. Filing blind does not leave a temporary file behind. On a long-lived
#      self-hosted runner those accumulate; codex measured 25 leaked per suite
#      run in round 1.
# --------------------------------------------------------------------------
state="$WORK/state-leak"
mkdir -p "$state" "$WORK/leak-tmp"
before="$(find "$WORK/leak-tmp" -maxdepth 1 -type f | wc -l)"
(
  cd "$WORK/tree"
  env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" "TMPDIR=$WORK/leak-tmp" \
    GH_FAKE_LIST_FAILS=1 FUZZ_ISSUE_SETTLE_SECONDS=0 \
    FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
    ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: leaky" \
      --body-file "$WORK/body.md" --comment-file "$WORK/comment.md"
) >/dev/null 2>&1
after="$(find "$WORK/leak-tmp" -maxdepth 1 -type f | wc -l)"
assert_eq "filing blind leaves no temporary file behind" "$before" "$after"

# --------------------------------------------------------------------------
# 12d. The WORKFLOW's title actually identifies the finding.
#
# Every check above drove the filer with hand-written titles, so the title the
# workflow builds was only ever tested by running the same input twice. A
# constant title with no target survived, and so did adding the artifact
# filename to the key (codex, round 2). These drive the lifted language-plugin
# shell, which is where the title is constructed.
# --------------------------------------------------------------------------
# The set is DERIVED: a workflow whose matrix carries a `target` dimension must
# put that target in its title, or two targets collapse onto one issue. Naming
# fuzz-language-plugins here instead made these checks fail outright on the
# sanitized mirror, which ships only fuzz-parse and has no target dimension.
#
# The derivation is cross-checked, so a workflow cannot leave the set by
# quietly dropping ${TARGET} from its title: having a matrix target is the
# independent signal, and it comes from a different part of the YAML than the
# title does.
mapfile -t target_workflows < <(
  "$PYTHON_BIN" - <<'DERIVE_TARGETS'
import pathlib, yaml

for path in sorted(pathlib.Path(".github/workflows").glob("fuzz-*.yml")):
    data = yaml.safe_load(path.read_text())
    if not isinstance(data, dict):
        continue
    for job in (data.get("jobs") or {}).values():
        steps = job.get("steps", [])
        if not any(s.get("name") == "File issue on crash" for s in steps):
            continue
        include = ((job.get("strategy") or {}).get("matrix") or {}).get("include") or []
        if any("target" in entry for entry in include):
            print(path.stem)
        break
DERIVE_TARGETS
)

expected_checks=$((expected_checks + ${#target_workflows[@]} * 2))
for workflow in "${target_workflows[@]}"; do
  wf_script="$WORK/$workflow.sh"
  if ! grep -q 'TARGET' "$wf_script"; then
    echo "FATAL: $workflow has a matrix target but its filing step never uses it," >&2
    echo "FATAL: so two targets would collapse onto one issue." >&2
    exit 1
  fi

  state="$WORK/state-title-$workflow"
  mkdir -p "$state"
  for target in svelte_plugin sql_plugin; do
    rm -rf "$WORK/tree/sqry-core"
    mkdir -p "$WORK/tree/sqry-core/fuzz/artifacts/$target"
    : >"$WORK/tree/sqry-core/fuzz/artifacts/$target/crash-aaaa"
    # A step that cannot run leaves the count wrong and the assertion below
    # says so. Letting `set -e` abort here instead meant one defect hid every
    # check after it, which is the coverage-disappearing shape round 6 kept
    # finding.
    run_step "$wf_script" "$state" 900000001 "$target" sqry-core/fuzz >/dev/null 2>&1 || true
  done
  assert_eq "$workflow: the title carries the target, so two targets are two issues" \
    2 "$(count_issues "$state")"

  # Two different crash artifacts for ONE target are one finding, so the
  # artifact filename must NOT be in the key.
  state="$WORK/state-artifact-$workflow"
  mkdir -p "$state"
  rm -rf "$WORK/tree/sqry-core"
  mkdir -p "$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin"
  : >"$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin/crash-aaaa"
  run_step "$wf_script" "$state" 900000002 svelte_plugin sqry-core/fuzz >/dev/null 2>&1 || true
  rm -f "$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin/crash-aaaa"
  : >"$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin/crash-bbbb"
  run_step "$wf_script" "$state" 900000003 svelte_plugin sqry-core/fuzz >/dev/null 2>&1 || true
  assert_eq "$workflow: the artifact filename is not part of the key" \
    1 "$(count_issues "$state")"
done

# Workflows that classify the finding KIND off the artifact prefix must keep a
# leak and a crash apart.
#
# The set was derived from the `leak-*)` classification arm, which is the very
# thing these checks test: deleting the arm removed the feature and both checks
# together and the suite went green at 91/0 while a leak artifact was filed
# under the generic failure title (codex, round 3). An enumeration that sits
# inside the blast radius of the change it constrains is not a check.
#
# The signal is now the artifact SEARCH: a step that looks for `leak-*` files
# has to say so in the title it builds. That is a different statement in a
# different part of the step from the case arm, so removing the arm leaves the
# workflow in the set and fails the check.
#
# The exclusion is inverted as well, because a derived set is necessary and not
# sufficient: a step that classifies leaks without searching for them would
# otherwise leave the set silently, so that combination is a failure rather
# than an exemption.
# EVERY artifact prefix the step searches for, not just leak. Covering only the
# leak arm left `timeout-*)` and `oom-*)` unprotected: deleting either one
# survived at 107/0 while the finding would be filed under the generic failure
# title (codex, round 5). The prefixes and their expected kind words are the
# pairs below; the SET under test is derived per workflow from the artifact
# search, which is a different statement from the case arm being tested.
# The artifact cases: the four prefixes libFuzzer writes, and a run with no
# artifact at all. DECLARED here rather than read back out of the workflow: the
# round-5 population was grepped from the lifted shell, so respelling one
# search removed the timeout case from the suite and the run went green one
# check shorter (codex, round 6). The round-6 repair declared the list but
# still counted the kind checks as one aggregate per workflow, so removing a
# member from the declaration removed its scenarios and its expectations
# together and the total held (codex, round 7). Each case is its own check now
# and CHECKS_PER_WORKFLOW spells the count out, so the declaration and the
# constant have to agree and one edit cannot move both.
KIND_CASES=(crash leak timeout oom none)
kind_word_for() {
  case "$1" in
    crash)   printf 'crash' ;;
    leak)    printf 'memory leak' ;;
    timeout) printf 'timeout' ;;
    oom)     printf 'OOM' ;;
    none)    printf 'failure (no crash artifact; likely infrastructure)' ;;
  esac
}
if ((${#KIND_CASES[@]} != KIND_CASE_COUNT)); then
  printf 'FATAL\tKIND_CASES has %s members and CHECKS_PER_WORKFLOW was derived for %s\n' \
    "${#KIND_CASES[@]}" "$KIND_CASE_COUNT" >&2
  exit 1
fi
# The count constant pins how MANY cases run, not WHICH. Replacing `timeout`
# with a second `crash` kept both constants honest and the suite green with
# four identities in five slots (grok, codex and cursor, round 8). Each case
# must be distinct and must be one this harness knows a title word for.
kind_cases_seen=""
for kind_case in "${KIND_CASES[@]}"; do
  case "$kind_cases_seen" in
    *"<$kind_case>"*)
      printf 'FATAL\tKIND_CASES repeats %s, so one artifact kind is not being run\n' "$kind_case" >&2
      exit 1
      ;;
  esac
  kind_cases_seen="$kind_cases_seen<$kind_case>"
  if [[ -z "$(kind_word_for "$kind_case")" ]]; then
    printf 'FATAL\tKIND_CASES names %s, which no kind word is declared for\n' "$kind_case" >&2
    exit 1
  fi
done

# The exact title each workflow must produce, stated here. Three of the four
# name the finding "crash" whatever the artifact is, and that is their design,
# not an accident: running every case against them asserts it. A filing
# workflow with no entry is fatal rather than uncovered, so adding one cannot
# quietly opt out of this.
expected_kind_title() {  # workflow kind-word target
  case "$1" in
    fuzz-critical)         printf 'fuzz-critical crash: %s' "$3" ;;
    fuzz-extended)         printf 'extended fuzz crash: %s' "$3" ;;
    fuzz-parse)            printf 'fuzz-parse crash detected' ;;
    fuzz-language-plugins) printf 'language plugin fuzz %s: %s' "$2" "$3" ;;
    *) return 1 ;;
  esac
}

kind_run_id=900000100
for workflow in "${filing_workflows[@]}"; do
  if ! expected_kind_title "$workflow" crash svelte_plugin >/dev/null 2>&1; then
    printf 'FATAL\t%s files issues and has no declared title; add it to expected_kind_title\n' \
      "$workflow" >&2
    exit 1
  fi
  state="$WORK/state-kind-$workflow"
  mkdir -p "$state"
  rm -rf "$WORK/tree/sqry-core"
  mkdir -p "$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin"

  # Each case twice, with a different run id and a different artifact file
  # name, so the check covers the RECURRENCE as well as the classification.
  # A single run per case only ever creates, so `kind="timeout-${RUN_ID}"`,
  # which puts the run id back in the deduplication key and is the exact defect
  # this whole gate exists for, produced a title that still CONTAINED the word
  # "timeout" and passed (codex, round 6). The no-artifact case is here for the
  # same reason: its fallback arm could carry the run id and nothing ran it
  # twice (codex, round 7).
  kind_titles_seen=""
  kind_want_titles=""
  for kind_case in "${KIND_CASES[@]}"; do
    want_title="$(expected_kind_title "$workflow" "$(kind_word_for "$kind_case")" svelte_plugin)"
    case "$kind_want_titles" in
      *"<$want_title>"*) ;;
      *) kind_want_titles="$kind_want_titles<$want_title>" ;;
    esac
    kind_transitions=""
    kind_want_transitions=""
    for repetition in 1 2; do
      rm -f "$WORK"/tree/sqry-core/fuzz/artifacts/svelte_plugin/*
      if [[ "$kind_case" != "none" ]]; then
        : >"$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin/${kind_case}-${repetition}bbbb"
      fi
      kind_run_id=$((kind_run_id + 1))
      kind_out="$(run_step "$WORK/$workflow.sh" "$state" "$kind_run_id" \
        svelte_plugin sqry-core/fuzz 2>/dev/null)" || kind_out="run-failed"
      kind_transitions="$kind_transitions${repetition}=$(awk '{print $1}' <<<"$kind_out") "
      # A title already filed must be commented on, never filed again.
      case "$kind_titles_seen" in
        *"<$want_title>"*) kind_want="commented" ;;
        *) kind_want="created"; kind_titles_seen="$kind_titles_seen<$want_title>" ;;
      esac
      kind_want_transitions="$kind_want_transitions${repetition}=${kind_want} "
    done
    assert_eq "$workflow: a ${kind_case} artifact creates once and then comments" \
      "$kind_want_transitions" "$kind_transitions"
  done
  # Whole titles, in creation order, not a substring of one of them.
  kind_actual_titles=""
  while IFS= read -r kind_title; do
    kind_actual_titles="$kind_actual_titles<$kind_title>"
  done < <(cut -f2 "$(issues_file "$state")")
  assert_eq "$workflow: each finding kind is titled exactly as declared" \
    "$kind_want_titles" "$kind_actual_titles"
  kind_want_count=0
  while [[ "$kind_want_titles" == *"<"* ]]; do
    kind_want_titles="${kind_want_titles#*>}"
    kind_want_count=$((kind_want_count + 1))
  done
  assert_eq "$workflow: each finding kind stays a separate issue" \
    "$kind_want_count" "$(count_issues "$state")"
done

# --------------------------------------------------------------------------
# 12e. Filing blind must not touch the CALLER's body file. Round 2's version
#      could append the disclosure to the original and pass, because nothing
#      checked the caller's file afterwards (codex, round 2).
# --------------------------------------------------------------------------
state="$WORK/state-caller-body"
mkdir -p "$state"
printf 'original caller body\n' >"$WORK/caller-body.md"
before_sum="$(cksum <"$WORK/caller-body.md")"
(
  cd "$WORK/tree"
  env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" GH_FAKE_LIST_FAILS=1 \
    FUZZ_ISSUE_SETTLE_SECONDS=0 FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 GH_TOKEN=fake \
    ./scripts/ci/file-fuzz-issue.sh --title "fuzz-critical crash: caller-body" \
      --body-file "$WORK/caller-body.md" --comment-file "$WORK/comment.md"
) >/dev/null 2>&1
assert_eq "filing blind leaves the caller's body file untouched" \
  "$before_sum" "$(cksum <"$WORK/caller-body.md")"
number="$(cut -f1 "$(issues_file "$state")")"
if grep -q "Deduplication was skipped" "$(gh_fake_bucket "$state")/body-$number.md"; then
  pass "the disclosure still reached the issue, from a copy"
else
  fail "the disclosure did not reach the issue"
fi

# --------------------------------------------------------------------------
# 12f. The production defaults are asserted, not just the flags' presence.
#      `--limit 500` to 30 and the retry backoff to 0 both survived round 2
#      because every scenario overrides them (grok).
# --------------------------------------------------------------------------
read_default() {  # variable-name default-pattern
  sed -n "s/^$1=\"\\\${$2:-\\([0-9]*\\)}\"$/\\1/p" scripts/ci/file-fuzz-issue.sh
}
default_limit="$(sed -n 's/^limit="\([0-9]*\)"$/\1/p' scripts/ci/file-fuzz-issue.sh)"
if [[ -n "$default_limit" ]] && ((default_limit >= 100)); then
  pass "the default lookup limit is well above gh's own 30 (${default_limit})"
else
  fail "the default lookup limit is ${default_limit:-unset}, at or near gh's default of 30"
fi
default_backoff="$(read_default retry_backoff_seconds FUZZ_ISSUE_RETRY_BACKOFF_SECONDS)"
if [[ -n "$default_backoff" ]] && ((default_backoff > 0)); then
  pass "the default retry backoff is non-zero (${default_backoff}s)"
else
  fail "the default retry backoff is ${default_backoff:-unset}, so retries would hammer"
fi

# --------------------------------------------------------------------------
# 12g. Both arms of the delimiter guard, and the --repo guard.
# --------------------------------------------------------------------------
state="$WORK/state-newline"
mkdir -p "$state"
reset_issues "$state"
set +e
file_issue "$state" "fuzz-critical crash: with
newline" >/dev/null 2>&1
newline_status=$?
set -e
assert_eq "a title containing a newline is refused before any lookup" 2 "$newline_status"

state="$WORK/state-repo"
mkdir -p "$state"
file_issue "$state" "fuzz-critical crash: repoflag" >/dev/null
repo_absent_values="$(gh_calls "$state" optall '*' --repo | sort -u | tr '\n' ',')"
if [[ "$repo_absent_values" == "<absent>," ]]; then
  pass "no --repo flag is passed when no repository is given"
else
  fail "the filer passed --repo when no repository was given: $repo_absent_values"
fi

# --------------------------------------------------------------------------
# 12h. A filing step leaves no temporary file behind. Each step makes two or
#      three with mktemp; on a long-lived self-hosted fuzz runner those
#      accumulate, and round 2 measured 24 per suite run (codex).
# --------------------------------------------------------------------------
state="$WORK/state-wf-leak"
mkdir -p "$state" "$WORK/wf-leak-tmp"
wf_leak_before="$(find "$WORK/wf-leak-tmp" -maxdepth 1 -type f | wc -l)"
for workflow in "${filing_workflows[@]}"; do
  rm -rf "$WORK/tree/sqry-core"
  mkdir -p "$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin"
  : >"$WORK/tree/sqry-core/fuzz/artifacts/svelte_plugin/crash-leaky"
  (
    cd "$WORK/tree"
    env "PATH=$WORK/bin:$PATH" "GH_FAKE_STATE=$state" "TMPDIR=$WORK/wf-leak-tmp" \
      FUZZ_ISSUE_SETTLE_SECONDS=0 FUZZ_ISSUE_RETRY_BACKOFF_SECONDS=0 \
      GH_TOKEN=fake RUN_ID=910000001 RUN_URL=https://example.invalid/r/910000001 \
      TARGET=svelte_plugin CRATE_DIR=sqry-core/fuzz \
      bash "$WORK/$workflow.sh"
  ) >/dev/null 2>&1
done
wf_leak_after="$(find "$WORK/wf-leak-tmp" -maxdepth 1 -type f | wc -l)"
assert_eq "a filing step leaves no temporary file behind" \
  "$wf_leak_before" "$wf_leak_after"

# --------------------------------------------------------------------------
# 12i. Both arms of the run-id guard are load bearing.
#
# Dropping the parenthesised arm survived round 3, because the historical
# titles all carry run ids long enough for the six-digit arm to catch anyway.
# A short run number exercises the parenthesised arm alone.
# --------------------------------------------------------------------------
state="$WORK/state-runid-arms"
mkdir -p "$state"
set +e
file_issue "$state" "fuzz-critical crash: shortrun (run 42)" >/dev/null 2>&1
short_runid_status=$?
file_issue "$state" "fuzz-critical crash: target run 33737060861" >/dev/null 2>&1
long_runid_status=$?
set -e
assert_eq "a parenthesised run id is refused even when the number is short" \
  2 "$short_runid_status"
assert_eq "an unparenthesised long run id is refused too" 2 "$long_runid_status"
# A refusal is only a refusal if nothing was written first. The exit status
# alone accepted the guard moved to AFTER the create: the issue was filed and
# the filer then exited 2 (codex, round 8). The state is fresh for this
# section, so both refusals together must have created nothing.
assert_eq "a refused title filed nothing and never called create" \
  "0/0" "$(count_issues "$state")/$(gh_calls "$state" count 'issue create')"
# ...and the guard must not fire on a word that merely ends in "run".
# The boundary is the round-2 repair: "overrun 1000000" and "longrun 33737060861"
# both END in run and are legitimate titles, not run ids (grok).
out="$(file_issue "$state" "language plugin fuzz crash: overrun 1000000")"
assert_eq "a word ending in run is not mistaken for a run id" \
  "created" "$(awk '{print $1}' <<<"$out")"
out="$(file_issue "$state" "language plugin fuzz crash: longrun 33737060861")"
assert_eq "a long digit string after a word ending in run is accepted too" \
  "created" "$(awk '{print $1}' <<<"$out")"

# --------------------------------------------------------------------------
# 12j. The inventory joins backslash line continuations before matching.
#
# Reverting it to a plain grep leaves no trace on this tree, because no
# shipped workflow splits the call. So the fixture is handed to the real
# scanner, not to a private copy of its grep: a plain-grep inventory then
# reports no offender here and this check fails.
# --------------------------------------------------------------------------
split_dir="$WORK/split-fixture"
mkdir -p "$split_dir"
cat >"$split_dir/fuzz-split.yml" <<'FIXTURE'
      - name: Report the crash
        run: |
          gh \
            issue create --title t --body b
FIXTURE
if [[ "$(list_direct_create_offenders "$split_dir")" == "$split_dir/fuzz-split.yml" ]]; then
  pass "a line-split gh issue create is still detected by the inventory"
else
  fail "a line-split gh issue create evades the inventory"
fi

# --------------------------------------------------------------------------
# 12k. The check-count assertion is itself checked, from outside.
#
# Round 2 called this survivor "not closable". Codex was right that it is only
# not SELF-detecting: an outer test that removes a check from a COPY and
# confirms the harness fails does close it. This is that test. It runs the copy
# against a one-workflow tree to stay fast, and FUZZ_HARNESS_META stops the
# copy from recursing into this block.
# --------------------------------------------------------------------------
if [[ -z "${FUZZ_HARNESS_META:-}" ]]; then
  # This block contributes its own checks to the expected total. Counting them
  # in FIXED_CHECKS instead made the meta-test vacuous: the copy skips this
  # block, so its expectation was lower and it absorbed the injected check
  # exactly. Three now: one per guard on the summary line, plus one for the
  # assertion path that feeds them.
  expected_checks=$((expected_checks + 3))

  # The copy runs against THIS tree, not a reduced one. A one-workflow tree
  # made the copy fail for reasons of its own, and the meta-test then could not
  # tell "the count assertion is dead" from "the copy failed on something
  # else". Same tree, one injected check, one expected diagnosis.
  # The source is the RUNNING script, not the repo-root-relative path. Reading
  # it from $REPO_ROOT meant that pointing FUZZ_HARNESS_REPO_ROOT at a second,
  # unmutated checkout made the meta-test grade that other tree and report
  # 93 checks, 0 failures over a harness whose count assertion was dead
  # (codex, round 3). ${BASH_SOURCE[0]} is whichever copy is executing.
  meta_inject() {  # injected-line out-path
    "$PYTHON_BIN" - "$HARNESS_SELF" "$2" "$1" <<'INJECT'
import pathlib, sys

src, dst, injected = sys.argv[1], sys.argv[2], sys.argv[3]
text = pathlib.Path(src).read_text()
marker = "printf '\\n%s checks, %s failures\\n'"
if marker not in text:
    raise SystemExit("summary line not found, so the meta-test cannot inject")
pathlib.Path(dst).write_text(text.replace(marker, injected + "\n" + marker, 1))
INJECT
  }

  meta_harness="$WORK/meta-harness.sh"
  meta_inject 'pass "meta injected check"' "$meta_harness"
  if FUZZ_HARNESS_META=1 FUZZ_HARNESS_REPO_ROOT="$REPO_ROOT" \
      bash "$meta_harness" >"$WORK/meta-out" 2>&1; then
    fail "a harness running one more check than it expects still passed"
  elif grep -q "checks ran;.*were expected" "$WORK/meta-out"; then
    pass "a harness running the wrong number of checks is caught from outside"
  else
    fail "the meta-copy failed, but not on the count: $(tail -1 "$WORK/meta-out")"
  fi

  # The summary prints TWO figures and both are guarded, but only the count
  # guard was covered: `if false && ((failures != 0))` survived at 103/0, and a
  # run reporting `103 checks, 1 failures` then exited 0 (codex, round 4).
  #
  # The injected line raises `failures` WITHOUT raising `checks`, so the count
  # guard stays silent and the failure guard is the only thing that can fail
  # the copy. A `fail` call would have moved both and proved neither.
  meta_fail_harness="$WORK/meta-fail-harness.sh"
  # shellcheck disable=SC2016  # literal, on purpose: it is injected shell text
  meta_inject 'failures=$((failures + 1))' "$meta_fail_harness"
  if FUZZ_HARNESS_META=1 FUZZ_HARNESS_REPO_ROOT="$REPO_ROOT" \
      bash "$meta_fail_harness" >"$WORK/meta-fail-out" 2>&1; then
    fail "a harness reporting a failure still exited zero"
    # This one cannot be left to the summary guard at the end of the file. The
    # copy is made from the RUNNING harness, so a dead failure guard is dead in
    # both, and the guard cannot fail the run about itself: the parent printed
    # `107 checks, 1 failures` and exited 0. The diagnosis exits here instead.
    echo "FATAL: the failure-count guard does not fail the run" >&2
    exit 1
  elif grep -q "of .* checks failed" "$WORK/meta-fail-out"; then
    pass "a harness reporting a failure is caught from outside"
  else
    fail "the meta-copy failed, but not on the failure count: $(tail -1 "$WORK/meta-fail-out")"
  fi

  # The check above raises `failures` directly, so it proves the final guard
  # and NOTHING about whether an ordinary assertion still records a failure.
  # Mutating fail() to `failures=$((failures + 0))` survived at 107/0, and a
  # run that printed a real FAIL line still exited 0 (codex, round 5).
  #
  # This injects a REAL failing assertion, and raises the expectation on the
  # same line so the count guard stays silent and only the failure path can
  # fail the copy. It also requires the copy to have PRINTED the FAIL line, so
  # a fail() that reports nothing is caught as well as one that counts nothing.
  meta_assert_harness="$WORK/meta-assert-harness.sh"
  # shellcheck disable=SC2016  # literal, on purpose: it is injected shell text
  meta_inject 'expected_checks=$((expected_checks + 1)); assert_eq "meta injected failing assertion" 1 2' \
    "$meta_assert_harness"
  if FUZZ_HARNESS_META=1 FUZZ_HARNESS_REPO_ROOT="$REPO_ROOT" \
      bash "$meta_assert_harness" >"$WORK/meta-assert-out" 2>&1; then
    fail "a failing assertion did not fail the harness"
    # Same reasoning as above: if fail() is what is broken, it cannot fail the
    # run about itself, so the diagnosis exits here.
    echo "FATAL: a failing assertion does not fail the run" >&2
    exit 1
  elif grep -q "^FAIL	meta injected failing assertion" "$WORK/meta-assert-out" \
      && grep -q "of .* checks failed" "$WORK/meta-assert-out"; then
    pass "a failing assertion is recorded and fails the harness"
  else
    fail "the meta-copy failed, but not on the assertion: $(tail -1 "$WORK/meta-assert-out")"
  fi
fi

# ==========================================================================
# 13. Every workflow that files a fuzz issue routes through the filer.
#
# This ran LAST in round 1, after the per-workflow lift, and `set -e` killed
# the harness at the lift before it was ever reached: the ADD-form negative
# control never exercised it. Its `pass` was also unconditional, so it
# incremented even when the loop above it had failed. Both are fixed: it is
# hoisted to the top of the run in scenario 0 and its verdict is bound to what
# the loop found (grok, round 1).
# ==========================================================================

printf '\n%s checks, %s failures\n' "$checks" "$failures"

# A run that asserted nothing is not a pass. Both figures the summary prints
# are asserted, so neither can read as coverage it did not provide.
if ((checks != expected_checks)); then
  echo "FATAL: ${checks} checks ran; ${expected_checks} were expected for ${#filing_workflows[@]} filing workflow(s)" >&2
  exit 1
fi
if ((failures != 0)); then
  echo "FATAL: ${failures} of ${checks} checks failed" >&2
  exit 1
fi
