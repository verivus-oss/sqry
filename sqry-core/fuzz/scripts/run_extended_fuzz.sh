#!/bin/bash
# Extended Fuzzing Run Script
# Runs all 37 fuzz targets in parallel batches
# Usage: ./run_extended_fuzz.sh [duration_seconds] [parallel_jobs]
#
# Examples:
#   ./run_extended_fuzz.sh              # 30 min per target, auto-detect parallelism
#   ./run_extended_fuzz.sh 1800 12      # 30 min, 12 parallel jobs
#   ./run_extended_fuzz.sh 3600 8       # 1 hour, 8 parallel jobs
#
# For multi-machine distribution, set FUZZ_MACHINE_ID and FUZZ_TOTAL_MACHINES:
#   FUZZ_MACHINE_ID=0 FUZZ_TOTAL_MACHINES=4 ./run_extended_fuzz.sh
#   FUZZ_MACHINE_ID=1 FUZZ_TOTAL_MACHINES=4 ./run_extended_fuzz.sh
#   ...

set -euo pipefail

# Configuration
DURATION=${1:-1800}  # 30 minutes default
PARALLEL_JOBS=${2:-$(( $(nproc) / 2 ))}  # Half of available cores
MAX_LEN=2048
MACHINE_ID=${FUZZ_MACHINE_ID:-0}
TOTAL_MACHINES=${FUZZ_TOTAL_MACHINES:-1}

# Fuzz targets (37 total)
TARGETS=(
    query_parser
    unified_parser
    c_plugin
    cpp_plugin
    csharp_plugin
    css_plugin
    dart_plugin
    elixir_plugin
    go_plugin
    groovy_plugin
    haskell_plugin
    html_plugin
    java_plugin
    javascript_plugin
    kotlin_plugin
    lua_plugin
    oracle_plsql_plugin
    perl_plugin
    php_plugin
    puppet_plugin
    python_plugin
    r_plugin
    ruby_plugin
    rust_plugin
    salesforce_apex_plugin
    sap_abap_plugin
    scala_plugin
    servicenow_xanadu_plugin
    shell_plugin
    sql_plugin
    svelte_plugin
    swift_plugin
    terraform_plugin
    typescript_plugin
    vue_plugin
    zig_plugin
    fuzz_target_1
)

TOTAL_TARGETS=${#TARGETS[@]}

# Calculate which targets this machine should run
get_machine_targets() {
    local targets=()
    for i in "${!TARGETS[@]}"; do
        if (( i % TOTAL_MACHINES == MACHINE_ID )); then
            targets+=("${TARGETS[$i]}")
        fi
    done
    echo "${targets[@]}"
}

# Results directory
RESULTS_DIR="fuzz/results/$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RESULTS_DIR"

# Log file
LOG_FILE="$RESULTS_DIR/fuzz_run.log"

log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "$LOG_FILE"
}

run_fuzz_target() {
    local target=$1
    local target_log="$RESULTS_DIR/${target}.log"
    local start_time=$(date +%s)

    log "Starting: $target (duration: ${DURATION}s)"

    # Run fuzzer and capture output
    if cargo +nightly fuzz run "$target" \
        --sanitizer=none \
        -- \
        -max_total_time="$DURATION" \
        -max_len="$MAX_LEN" \
        > "$target_log" 2>&1; then
        local status="SUCCESS"
    else
        local status="FAILED"
    fi

    local end_time=$(date +%s)
    local elapsed=$((end_time - start_time))

    # Extract metrics from log
    local coverage=$(grep -oP 'cov: \K\d+' "$target_log" | tail -1 || echo "N/A")
    local corpus=$(grep -oP 'corp: \K\d+' "$target_log" | tail -1 || echo "N/A")
    local execs=$(grep -oP '#\K\d+' "$target_log" | tail -1 || echo "N/A")

    # Check for crashes
    local crashes=0
    if ls fuzz/artifacts/"$target"/crash-* 2>/dev/null | head -1 > /dev/null; then
        crashes=$(ls fuzz/artifacts/"$target"/crash-* 2>/dev/null | wc -l)
        status="CRASH"
    fi

    log "Completed: $target | Status: $status | Time: ${elapsed}s | Coverage: $coverage | Corpus: $corpus | Execs: $execs | Crashes: $crashes"

    # Write summary line
    echo "$target,$status,$elapsed,$coverage,$corpus,$execs,$crashes" >> "$RESULTS_DIR/summary.csv"
}

export -f run_fuzz_target log
export RESULTS_DIR LOG_FILE DURATION MAX_LEN

# Main
cd "$(dirname "$0")/../.."  # Go to sqry-core directory

log "=========================================="
log "Extended Fuzzing Run"
log "=========================================="
log "Duration per target: ${DURATION}s ($(( DURATION / 60 )) minutes)"
log "Parallel jobs: $PARALLEL_JOBS"
log "Max input length: $MAX_LEN"
log "Machine: $MACHINE_ID / $TOTAL_MACHINES"
log "Results directory: $RESULTS_DIR"
log "=========================================="

# Get targets for this machine
if (( TOTAL_MACHINES > 1 )); then
    MACHINE_TARGETS=($(get_machine_targets))
    log "This machine will run ${#MACHINE_TARGETS[@]} targets: ${MACHINE_TARGETS[*]}"
else
    MACHINE_TARGETS=("${TARGETS[@]}")
    log "Running all $TOTAL_TARGETS targets"
fi

# Initialize CSV
echo "target,status,elapsed_seconds,coverage,corpus_size,executions,crashes" > "$RESULTS_DIR/summary.csv"

# Calculate estimated time
NUM_TARGETS=${#MACHINE_TARGETS[@]}
BATCHES=$(( (NUM_TARGETS + PARALLEL_JOBS - 1) / PARALLEL_JOBS ))
ESTIMATED_MINUTES=$(( BATCHES * DURATION / 60 ))
log "Estimated wall time: ~${ESTIMATED_MINUTES} minutes (${BATCHES} batches)"
log "=========================================="
log ""

# Run targets in parallel using GNU parallel or xargs
START_TIME=$(date +%s)

if command -v parallel &> /dev/null; then
    log "Using GNU parallel with $PARALLEL_JOBS jobs"
    printf '%s\n' "${MACHINE_TARGETS[@]}" | parallel -j "$PARALLEL_JOBS" run_fuzz_target {}
else
    log "Using xargs with $PARALLEL_JOBS jobs"
    printf '%s\n' "${MACHINE_TARGETS[@]}" | xargs -P "$PARALLEL_JOBS" -I {} bash -c 'run_fuzz_target "$@"' _ {}
fi

END_TIME=$(date +%s)
TOTAL_ELAPSED=$((END_TIME - START_TIME))

log ""
log "=========================================="
log "Fuzzing Run Complete"
log "=========================================="
log "Total wall time: ${TOTAL_ELAPSED}s ($(( TOTAL_ELAPSED / 60 )) minutes)"
log "Results: $RESULTS_DIR/summary.csv"
log ""

# Print summary
log "Summary:"
echo ""
column -t -s',' "$RESULTS_DIR/summary.csv" | tee -a "$LOG_FILE"
echo ""

# Count results
TOTAL_SUCCESS=$(grep -c ",SUCCESS," "$RESULTS_DIR/summary.csv" || echo 0)
TOTAL_CRASH=$(grep -c ",CRASH," "$RESULTS_DIR/summary.csv" || echo 0)
TOTAL_FAILED=$(grep -c ",FAILED," "$RESULTS_DIR/summary.csv" || echo 0)

log "=========================================="
log "Results: $TOTAL_SUCCESS success, $TOTAL_CRASH crashes, $TOTAL_FAILED failed"
log "=========================================="

# Exit with error if any crashes found
if (( TOTAL_CRASH > 0 )); then
    log "WARNING: Crashes detected! Check fuzz/artifacts/ for crash inputs."
    exit 1
fi

exit 0
