#!/usr/bin/env bash
#
# check_phase_a_perf_gate.sh: enforce the SPEC §5.2 / DESIGN §14.4 +15%
# build-time gate for Phase A C indirect-call precision (U19).
#
# Methodology (2026-07-06 remethodology, verivus-oss/sqry#280):
#   The gate measures the SAME-COMMIT MARGINAL cost of Phase A: it builds the
#   linux-driver-subset fixture with Phase A on and off at the current commit,
#   many times, and enforces the PAIRED per-pair ratio's median:
#     paired_marginal = median_i[ (with_i - without_i) / without_i ] <= +15%
#
#   This is deliberately the median of the PER-PAIR ratio, not the ratio of the
#   two arms' separate medians (`(median(with) - median(without)) /
#   median(without)`). A same-day review of an earlier version of this gate
#   found the separate-median formula could mask a real regression: on a
#   contended host, that statistic collapsed to a small number while the true,
#   paired marginal (and the min cross-check) stayed high, i.e. a false PASS.
#   `median(A) - median(B)` only equals `median(A - B)` when independently
#   sorting each arm preserves every pair's relative rank, which independent
#   per-measurement timing noise does not guarantee. Computing the ratio pair
#   by pair, before either arm is sorted, is what makes common-mode drift
#   cancel and closes that false-negative path; see
#   `sqry-lang-c/examples/phase_a_marginal.rs` module doc and its
#   `paired_statistic_catches_regression_separate_median_would_miss` test for
#   the full argument and a worked counterexample.
#
#   The "with" build is the full build; the "without" (floor) build skips only
#   the three Phase A C-plugin instrumentation walks (address-taken
#   classification, the local scope index, the known-function-names leg) via
#   `sqry_lang_c::CPlugin::without_phase_a`. The callsite capture and the core
#   `pass5b_c_indirect` pass run in both arms, so the marginal isolates the
#   SPEC §5.2 "C plugin build time" cost, matching PR #351's floor method.
#
#   The measurement is a PAIRED, ALTERNATING loop (the
#   `sqry-lang-c/examples/phase_a_marginal.rs` binary), not two separate
#   criterion benchmarks. The instrumentation marginal is roughly 1 ms on a
#   roughly 11 ms build; two whole-build criterion means measured tens of
#   seconds apart each carry 5 to 15 percent run-to-run variance (CPU frequency
#   scaling, thermal drift, page cache, host contention), which buries the
#   signal. The example instead times the on and off builds back-to-back, many
#   times, alternating which runs first, so slow drift cancels pair by pair and
#   the marginal is reproducible on any quiet host. The per-arm medians, the
#   min-based marginal, and the separate-median ratio are still reported by
#   the example for observability, but none of them feed the PASS/FAIL
#   decision below; only the paired per-pair ratio's median does.
#
#   `CPlugin::without_phase_a` is exposed only under the `phase-a-toggle` cargo
#   feature (never in production builds). This replaces the earlier gate, which
#   compared a fresh criterion mean against the frozen absolute
#   `measurements/2026-05-14-bench-baseline.json` captured at commit 02799e8c5.
#   That frozen number drifted with host and non-Phase-A pipeline growth (v15 to
#   v27) and could no longer isolate the Phase A cost; it is retained under
#   `measurements/` as a historical record only and is no longer read here.
#
# Note: run on a reasonably quiet host. Like any perf gate it is sensitive to
# concurrent CPU load. The paired design removes drift, not gross contention.
#
# Usage:
#   scripts/measure/check_phase_a_perf_gate.sh   # measure + enforce the gate
#   scripts/measure/check_phase_a_perf_gate.sh --self-test   # math self-test
#
# Environment overrides:
#   SQRY_PHASE_A_GATE_BUDGET   marginal budget fraction        (default 0.15)
#   SQRY_PHASE_A_GATE_ITERS    paired iterations               (default 50)
#   SQRY_PHASE_A_GATE_WARMUP   discarded warmup pairs          (default 5)
#
# Exit codes:
#   0  marginal within budget (PASS)
#   1  marginal exceeds budget (FAIL)
#   2  required command missing on PATH / bad usage
#   3  measurement failed / output malformed
#   4  --self-test assertion failure

set -euo pipefail

# ----------------------------------------------------------------------------
# Constants / config
# ----------------------------------------------------------------------------

THIS_SCRIPT="${BASH_SOURCE[0]}"
THIS_DIR="$(cd "$(dirname "$THIS_SCRIPT")" && pwd)"
REPO_ROOT="$(cd "$THIS_DIR/../.." && pwd)"

BUDGET="${SQRY_PHASE_A_GATE_BUDGET:-0.15}"

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

# ----------------------------------------------------------------------------
# Measurement-driven gate
# ----------------------------------------------------------------------------

# Run the paired-alternating measurement example and echo its stdout (the
# machine-readable PAIRED_MARGINAL / WITH_NS / WITHOUT_NS /
# MARGINAL_SEPARATE_MEDIAN lines).
run_measurement() {
    ( cd "$REPO_ROOT" && cargo run --quiet --release \
        --example phase_a_marginal \
        --features phase-a-toggle )
}

# Extract `KEY=<value>` from the measurement output. Exits 3 if absent.
extract_field() {
    local key="$1"
    local text="$2"
    local val
    val="$(printf '%s\n' "$text" | awk -F= -v k="$key" '$1 == k { print $2; exit }')"
    if [[ -z "$val" ]]; then
        err "measurement output missing $key line"
        exit 3
    fi
    printf '%s' "$val"
}

# Apply the gate predicate to the paired per-pair marginal ratio -- the
# ENFORCED statistic (see header comment). with_ns/without_ns/
# marginal_separate_median are cross-check display values only and never feed
# the PASS/FAIL decision: that separation is the entire fix for
# verivus-oss/sqry#280, since the separate-median ratio can read well
# within budget on a contended host while the true, paired marginal exceeds
# it. Returns 0 on PASS, 1 on FAIL.
run_gate() {
    local paired_marginal="$1"
    local with_ns="$2"
    local without_ns="$3"
    local marginal_separate_median="$4"

    local pct budget_pct sep_pct
    pct="$(awk -v r="$paired_marginal" 'BEGIN { printf "%+.2f", r * 100.0 }')"
    budget_pct="$(awk -v b="$BUDGET" 'BEGIN { printf "%.2f", b * 100.0 }')"
    sep_pct="$(awk -v r="$marginal_separate_median" 'BEGIN { printf "%+.2f", r * 100.0 }')"

    printf 'Phase A instrumentation marginal build-time delta (paired per-pair ratio, median):\n'
    printf '  with    (full, median)                       : %.3f ms\n' \
        "$(awk -v v="$with_ns" 'BEGIN { printf "%.6f", v / 1000000.0 }')"
    printf '  without (floor, median)                      : %.3f ms\n' \
        "$(awk -v v="$without_ns" 'BEGIN { printf "%.6f", v / 1000000.0 }')"
    printf '  paired marginal (ENFORCED)                   : %s%%\n' "$pct"
    printf '  separate-median marginal (cross-check only)  : %s%%\n' "$sep_pct"
    printf '  budget                                       : +%s%%\n' "$budget_pct"

    local within
    within="$(awk -v r="$paired_marginal" -v b="$BUDGET" 'BEGIN { print (r <= b) ? "1" : "0" }')"
    if [[ "$within" == "1" ]]; then
        printf 'PASS: paired marginal %s%% within +%s%% budget\n' "$pct" "$budget_pct"
        return 0
    fi
    printf 'FAIL: paired marginal %s%% exceeds +%s%% budget\n' "$pct" "$budget_pct"
    return 1
}

gate_from_measurement() {
    require_cmd cargo
    require_cmd awk

    local output
    if ! output="$(run_measurement)"; then
        err "measurement binary failed to run"
        exit 3
    fi

    local paired_marginal with_ns without_ns marginal_separate_median
    paired_marginal="$(extract_field PAIRED_MARGINAL "$output")"
    with_ns="$(extract_field WITH_NS "$output")"
    without_ns="$(extract_field WITHOUT_NS "$output")"
    marginal_separate_median="$(extract_field MARGINAL_SEPARATE_MEDIAN "$output")"

    run_gate "$paired_marginal" "$with_ns" "$without_ns" "$marginal_separate_median"
}

# ----------------------------------------------------------------------------
# Self-test (pure ratio math, no cargo)
# ----------------------------------------------------------------------------

self_test() {
    require_cmd awk

    local status

    printf 'self-test 1/4: +31%% paired marginal must FAIL the gate\n'
    set +e
    BUDGET=0.15 run_gate 0.31 13100000 10000000 0.31
    status=$?
    set -e
    if [[ "$status" -eq 0 ]]; then
        err "self-test failed: +31% paired marginal should exit non-zero, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected non-zero) -- OK\n' "$status"

    printf 'self-test 2/4: +10%% paired marginal must PASS the gate\n'
    set +e
    BUDGET=0.15 run_gate 0.10 11000000 10000000 0.10
    status=$?
    set -e
    if [[ "$status" -ne 0 ]]; then
        err "self-test failed: +10% paired marginal should exit 0, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected 0) -- OK\n' "$status"

    printf 'self-test 3/4: +15.00%% exact boundary must PASS the gate (<= 15%%)\n'
    set +e
    BUDGET=0.15 run_gate 0.15 11500000 10000000 0.15
    status=$?
    set -e
    if [[ "$status" -ne 0 ]]; then
        err "self-test failed: +15.00% exact should exit 0, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected 0) -- OK\n' "$status"

    printf 'self-test 4/4: paired statistic must FAIL a regression the separate-median statistic would have PASSED\n'
    printf '  (reproduces the verivus-oss/sqry#280 false-negative: separate-median +9.35%% PASSes,\n'
    printf '   the true paired marginal +25.90%% correctly FAILs the same data)\n'
    set +e
    BUDGET=0.15 run_gate 0.259049 14371734 13142542 0.093528
    status=$?
    set -e
    if [[ "$status" -eq 0 ]]; then
        err "self-test failed: a +25.90% paired marginal should exit non-zero even though" \
            "the separate-median cross-check (+9.35%) would have passed, got $status"
        exit 4
    fi
    printf '  -> got exit=%s (expected non-zero) -- OK: the gate keyed off the paired statistic, not the separate-median one\n' "$status"

    printf 'self-test complete: paired marginal gate predicate is falsifiable and immune to the separate-median false negative.\n'
}

# ----------------------------------------------------------------------------
# CLI dispatch
# ----------------------------------------------------------------------------

usage() {
    cat <<EOF
Usage:
  $THIS_SCRIPT              measure the paired same-commit marginal, enforce gate
  $THIS_SCRIPT --self-test  validate the ratio math offline

  Enforces the SPEC §5.2 +${BUDGET} same-commit marginal build-time budget for
  Phase A C indirect-call precision. See the header comment for methodology.
EOF
}

main() {
    if [[ $# -eq 0 ]]; then
        gate_from_measurement
        return
    fi
    case "$1" in
        --self-test)
            self_test
            ;;
        -h | --help)
            usage
            ;;
        *)
            usage >&2
            exit 2
            ;;
    esac
}

main "$@"
