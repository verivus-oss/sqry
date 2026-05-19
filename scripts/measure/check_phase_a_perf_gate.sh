#!/usr/bin/env bash
#
# check_phase_a_perf_gate.sh — enforce SPEC §5.2 / DESIGN §14.4 +30%
# build-time gate for Phase A C indirect-call precision (U19).
#
# Usage:
#   scripts/measure/check_phase_a_perf_gate.sh <baseline-json> <post-json>
#
# Each JSON file is the aggregated output of running the
# `sqry-lang-c/benches/c_indirect.rs` criterion bench. The script reads
# `.benches.bench_full_build_linux_fs_subset.mean_ns` from each file,
# computes `delta = (post - pre) / pre`, prints a PASS/FAIL line, and
# exits 0 if `delta <= 0.30`, exits 1 otherwise.
#
# Self-test:
#   scripts/measure/check_phase_a_perf_gate.sh --self-test
#
# The self-test synthesises a doctored bench output where
# `bench_full_build_linux_fs_subset.mean_ns` is 1.31× the baseline and
# asserts the gate fires (exits non-zero), then synthesises a +5%
# regression and asserts the gate passes (exits zero), plus a +30.00%
# boundary case.
#
# Per CLAUDE.md "sqry-First Workflow": this script is the falsifiable
# gate that turns SPEC §5.2's "+30% build time" target (amended
# 2026-05-19 from the original +15%; see SPEC §5.2 Amendment + the
# verivus-oss/sqry#280 follow-up) from aspirational into enforced.
# CI matrix gate #7 invokes this script (see
# `docs/development/c-semantic-phase-a-icall-precision/03_IMPLEMENTATION_PLAN-c-semantic-phase-a-icall-precision.md`
# §4 row 7).

set -euo pipefail

# ----------------------------------------------------------------------------
# Helpers
# ----------------------------------------------------------------------------

THIS_SCRIPT="${BASH_SOURCE[0]}"
THIS_DIR="$(cd "$(dirname "$THIS_SCRIPT")" && pwd)"

err() {
    printf 'error: %s\n' "$*" >&2
}

require_cmd() {
    local cmd="$1"
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "required command not found on PATH: $cmd"
        exit 2
    fi
}

# Extract `.benches.bench_full_build_linux_fs_subset.mean_ns` from a
# JSON file. Exits with code 3 if the field is missing or not numeric.
extract_mean_ns() {
    local json_path="$1"
    if [[ ! -f "$json_path" ]]; then
        err "JSON file not found: $json_path"
        exit 3
    fi
    local val
    val="$(jq -r '.benches.bench_full_build_linux_fs_subset.mean_ns // empty' "$json_path")"
    if [[ -z "$val" || "$val" == "null" ]]; then
        err "missing or null .benches.bench_full_build_linux_fs_subset.mean_ns in $json_path"
        exit 3
    fi
    printf '%s' "$val"
}

# Compute (post - pre) / pre using awk's floating-point math (always
# available, no `bc` dependency required). Prints the ratio to stdout.
compute_delta_ratio() {
    local pre="$1"
    local post="$2"
    awk -v pre="$pre" -v post="$post" 'BEGIN {
        if (pre + 0 == 0) {
            print "inf";
            exit;
        }
        printf "%.10f", (post - pre) / pre;
    }'
}

# Run the gate logic on two JSON files. Returns 0 if delta <= 0.30,
# returns 1 otherwise. Prints a PASS/FAIL line to stdout.
run_gate() {
    local baseline_json="$1"
    local post_json="$2"

    local pre_mean
    local post_mean
    pre_mean="$(extract_mean_ns "$baseline_json")"
    post_mean="$(extract_mean_ns "$post_json")"

    local delta_ratio
    delta_ratio="$(compute_delta_ratio "$pre_mean" "$post_mean")"

    local delta_pct
    delta_pct="$(awk -v r="$delta_ratio" 'BEGIN { printf "%+.2f", r * 100.0 }')"

    # Compare delta_ratio <= 0.30 via awk; 0 on PASS, 1 on FAIL.
    local within_budget
    within_budget="$(awk -v r="$delta_ratio" 'BEGIN { print (r <= 0.30) ? "1" : "0" }')"

    if [[ "$within_budget" == "1" ]]; then
        printf 'PASS: bench_full_build_linux_fs_subset delta = %s%% (within +30.00%% budget)\n' "$delta_pct"
        return 0
    else
        printf 'FAIL: bench_full_build_linux_fs_subset delta = %s%% (exceeds +30.00%% budget)\n' "$delta_pct"
        return 1
    fi
}

# ----------------------------------------------------------------------------
# Self-test
# ----------------------------------------------------------------------------

self_test() {
    require_cmd jq
    require_cmd awk

    local tmpdir
    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' RETURN

    local baseline="$tmpdir/baseline.json"
    local post_regress="$tmpdir/post-regress.json"
    local post_ok="$tmpdir/post-ok.json"
    local post_exact_30="$tmpdir/post-exact-30.json"

    cat >"$baseline" <<'EOF'
{
  "schema_version": 1,
  "captured_against": "self-test-baseline",
  "benches": {
    "bench_build_local_scope_index": { "mean_ns": 1000.0 },
    "bench_pass5b_resolve_synthetic": { "mean_ns": 2000.0 },
    "bench_full_build_linux_fs_subset": { "mean_ns": 1000000.0 }
  }
}
EOF

    # +31% regression — must fail.
    cat >"$post_regress" <<'EOF'
{
  "schema_version": 1,
  "captured_against": "self-test-post-regress",
  "benches": {
    "bench_build_local_scope_index": { "mean_ns": 1000.0 },
    "bench_pass5b_resolve_synthetic": { "mean_ns": 2000.0 },
    "bench_full_build_linux_fs_subset": { "mean_ns": 1310000.0 }
  }
}
EOF

    # +5% — must pass.
    cat >"$post_ok" <<'EOF'
{
  "schema_version": 1,
  "captured_against": "self-test-post-ok",
  "benches": {
    "bench_build_local_scope_index": { "mean_ns": 1000.0 },
    "bench_pass5b_resolve_synthetic": { "mean_ns": 2000.0 },
    "bench_full_build_linux_fs_subset": { "mean_ns": 1050000.0 }
  }
}
EOF

    # +30.00% exactly — boundary case; the gate is `<= 0.30`, so this must pass.
    cat >"$post_exact_30" <<'EOF'
{
  "schema_version": 1,
  "captured_against": "self-test-post-exact-30",
  "benches": {
    "bench_build_local_scope_index": { "mean_ns": 1000.0 },
    "bench_pass5b_resolve_synthetic": { "mean_ns": 2000.0 },
    "bench_full_build_linux_fs_subset": { "mean_ns": 1300000.0 }
  }
}
EOF

    local status

    printf 'self-test 1/3: +31%% regression must FAIL the gate\n'
    set +e
    run_gate "$baseline" "$post_regress"
    status=$?
    set -e
    if [[ "$status" -eq 0 ]]; then
        err "self-test failed: +31%% regression should have exited non-zero, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected non-zero) -- OK\n' "$status"

    printf 'self-test 2/3: +5%% under budget must PASS the gate\n'
    set +e
    run_gate "$baseline" "$post_ok"
    status=$?
    set -e
    if [[ "$status" -ne 0 ]]; then
        err "self-test failed: +5%% should have exited 0, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected 0) -- OK\n' "$status"

    printf 'self-test 3/3: +30.00%% exact boundary must PASS the gate (<= 30%%)\n'
    set +e
    run_gate "$baseline" "$post_exact_30"
    status=$?
    set -e
    if [[ "$status" -ne 0 ]]; then
        err "self-test failed: +30.00%% exact should have exited 0, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected 0) -- OK\n' "$status"

    printf 'self-test complete: gate predicate is falsifiable as expected.\n'
}

# ----------------------------------------------------------------------------
# CLI dispatch
# ----------------------------------------------------------------------------

usage() {
    cat <<EOF
Usage:
  $THIS_SCRIPT <baseline-json> <post-json>
  $THIS_SCRIPT --self-test

  Enforces the SPEC §5.2 +30% build-time gate for the
  bench_full_build_linux_fs_subset criterion bench. Both JSON files must
  carry .benches.bench_full_build_linux_fs_subset.mean_ns.

  Exit codes:
    0  delta within +30% budget (PASS)
    1  delta exceeds +30% budget (FAIL)
    2  required command missing on PATH (jq, awk)
    3  JSON file missing / malformed / missing required field
    4  --self-test assertion failure
EOF
}

main() {
    if [[ $# -eq 1 && "$1" == "--self-test" ]]; then
        self_test
        exit 0
    fi
    if [[ $# -ne 2 ]]; then
        usage >&2
        exit 2
    fi
    require_cmd jq
    require_cmd awk
    run_gate "$1" "$2"
}

main "$@"
