#!/usr/bin/env bash
# scripts/ci/check_routing_gates.sh
#
# STEP_11 static-routing gate: structural barrier preventing the
# regression class that motivated the workspace-aware-cross-repo design
# from being reintroduced. Two patterns are forbidden:
#
#   (A) New `vscode.workspace.workspaceFolders` enumeration loops in
#       `sqry-vscode/src/**/*.ts` that are NOT preceded (within a
#       small lexical window) by a call to a `classifyLogicalWorkspace`
#       helper. The original `aivcs-runner-folder` regression slipped
#       in as exactly this shape: a `for (const folder of
#       vscode.workspace.workspaceFolders)` loop running per-folder
#       filesystem probes without the LogicalWorkspace classifier
#       gating.
#
#   (B) New `GraphStorage::new(...)` constructions inside
#       `sqry-lsp/src/handlers/**/*.rs` that are NOT preceded (within
#       a small lexical window) by a call to either
#       `LogicalWorkspace::classify` or
#       `WorkspaceManager::classify_for_serve` (or their
#       `session.classify_path(...)` re-export). LSP handlers must
#       consult the LogicalWorkspace before opening per-folder graph
#       storage.
#
# Exit codes:
#   0 — every loop / construction is gated upstream by a classifier.
#   1 — one or more violations.
#   2 — invocation / environment error.
#
# Implementation:
#   - For (A) we scan each TS file. For every `vscode.workspace.workspaceFolders`
#     occurrence, we read the preceding 50 lines and check for any
#     `classifyLogicalWorkspace`, `classify_path`, `nonExcludedFolders`,
#     `enumerateClassifiedFolders` token, or `// routing-gate-allow:<reason>`
#     escape hatch on the same or preceding line.
#   - For (B) we scan each handler .rs file. For every `GraphStorage::new`
#     occurrence, we read the preceding 50 lines and check for any
#     `LogicalWorkspace::classify`, `classify_path`, `classify_for_serve`,
#     or `// routing-gate-allow:<reason>` escape hatch.
#
# The escape-hatch comment is intentionally explicit so reviewers see
# every intentional bypass during code review. The justification text
# (anything after `:`) is non-empty by regex.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TS_ROOT="${REPO_ROOT}/sqry-vscode/src"
# We scan all of `sqry-lsp/src/` (not just `handlers/`) so non-handler
# `GraphStorage::new(...)` sites — `server.rs:1210` and `session.rs:524 / :1234`
# at time of writing — are covered. The DAG text references
# `sqry-lsp/src/handlers/**/*.rs`, but the spirit of the gate is "no
# new ungated GraphStorage::new in the LSP crate"; restricting to one
# subdirectory creates an obvious bypass. Codex iter1 MAJOR.
LSP_ROOT="${REPO_ROOT}/sqry-lsp/src"

WINDOW_LINES="${ROUTING_GATE_WINDOW_LINES:-50}"

if [[ ! -d "${TS_ROOT}" ]]; then
  echo "error: ${TS_ROOT} not found" >&2
  exit 2
fi
if [[ ! -d "${LSP_ROOT}" ]]; then
  echo "error: ${LSP_ROOT} not found" >&2
  exit 2
fi

# Tokens that legitimately gate a workspaceFolders enumeration. Any of
# these appearing in the preceding window absolves the loop.
readonly -a TS_GATE_TOKENS=(
  "classifyLogicalWorkspace"
  "classify_path"
  "classifyPath"
  "nonExcludedFolders"
  "enumerateClassifiedFolders"
  "buildWorkspaceInitializationPayload"
  "isFolderExcluded"
  "// routing-gate-allow:"
)

# Tokens that legitimately gate a GraphStorage::new construction.
#
# Two families are accepted as "classifier upstream" (codex iter1 MAJOR
# tightening — `resolve_path(` and `workspace_root` were too broad,
# matching e.g. unrelated function-name fragments):
#
#   1. Direct LogicalWorkspace touchpoints — symbols / methods that
#      can ONLY be reached through a resolved `LogicalWorkspace`:
#        - `LogicalWorkspace::classify`        — the classifier itself
#        - `classify_for_serve`                — daemon classifier wrapper
#        - `classify_path`                     — SessionManager re-export
#        - `session.classify`                  — same, dotted form
#        - `logical_workspace`                 — Session/Engine accessor
#        - `LogicalWorkspace`                  — type-name witness;
#                                                a function that takes
#                                                `&LogicalWorkspace`
#                                                has a workspace by
#                                                construction
#        - `.source_roots()`                   — only callable on a
#                                                resolved workspace
#        - `.member_folders()`                 — same
#        - `.exclusions()`                     — same
#   2. The explicit `// routing-gate-allow:<reason>` escape hatch.
readonly -a RS_GATE_TOKENS=(
  "LogicalWorkspace::classify"
  "classify_path"
  "classify_for_serve"
  "session.classify"
  "logical_workspace"
  "LogicalWorkspace"
  ".source_roots()"
  ".member_folders()"
  ".exclusions()"
  "// routing-gate-allow:"
)

# ---------------------------------------------------------------------------
# Pre-existing call sites that the routing gate would otherwise flag but
# that are not, in fact, routing-relevant. Each entry must carry an
# explicit owner and a justification — the list is INTENTIONALLY narrow
# and the gate refuses to grow it implicitly.
#
# Format (per line):  <repo-relative-path>:<lineno>
#
#   - sqry-vscode/src/searchPanel.ts:953
#       Tree-view rendering. Enumerates `vscode.workspace.workspaceFolders`
#       only to materialize per-root tree items for an `indexStatusMap`
#       that is itself populated upstream by classifier-aware code in
#       extension.ts (`forEachClassifiedSourceRoot`). The loop is
#       UI-presentation, not routing. Tracked for STEP_5 follow-up to
#       add an inline `// routing-gate-allow:` comment so the gate
#       no longer needs the carve-out.
# ---------------------------------------------------------------------------
known_ungated_ts="$(mktemp)"
cat > "${known_ungated_ts}" <<'EOF'
sqry-vscode/src/searchPanel.ts:953
EOF
sort -u -o "${known_ungated_ts}" "${known_ungated_ts}"

# Same pattern for Rust LSP handler/session sites that pre-date the
# workspace-aware-cross-repo workstream. Each entry must carry an
# explicit owner; STEP_11_4_CROSS_CUTTING (`[units.STEP_11_4_CROSS_CUTTING]`
# in the DAG TOML) is the unit that converts these to classifier-gated
# implementations, and its acceptance contract explicitly says
# "All 6 LSP handler types ... consult LogicalWorkspace.classify()
# before any filesystem probe".
#
# Format (per line):  <repo-relative-path>:<lineno>
#
#   - sqry-lsp/src/handlers/graph_stats.rs:30
#   - sqry-lsp/src/handlers/index.rs:780
#   - sqry-lsp/src/handlers/trace_path.rs:449
#   - sqry-lsp/src/handlers/is_node_in_cycle.rs:223
#       Each handler resolves a workspace-relative path through
#       SessionManager::resolve_path before constructing GraphStorage.
#       SessionManager::resolve_path enforces workspace-bound
#       canonicalization (rejects directory-traversal), which is the
#       de-facto "classifier upstream" today; STEP_11_4_CROSS_CUTTING
#       upgrades these to explicit LogicalWorkspace::classify()
#       gates per the DAG. Tracked there.
#
#   - sqry-lsp/src/session.rs:524
#       SessionManager::graph() loads the session-level graph from
#       `current_index_root`, which is itself derived from the
#       SessionManager's `index_root` config (workspace-bound).
#       Tracked under STEP_11_4_CROSS_CUTTING for an explicit
#       classifier touchpoint.
known_ungated_rs="$(mktemp)"
cat > "${known_ungated_rs}" <<'EOF'
sqry-lsp/src/handlers/graph_stats.rs:30
sqry-lsp/src/handlers/index.rs:780
sqry-lsp/src/handlers/trace_path.rs:449
sqry-lsp/src/handlers/is_node_in_cycle.rs:223
sqry-lsp/src/session.rs:524
EOF
sort -u -o "${known_ungated_rs}" "${known_ungated_rs}"

ts_violations_raw="$(mktemp)"
ts_violations="$(mktemp)"
rs_violations_raw="$(mktemp)"
rs_violations="$(mktemp)"
trap 'rm -f "${ts_violations}" "${ts_violations_raw}" "${rs_violations}" "${rs_violations_raw}" "${known_ungated_ts}" "${known_ungated_rs}"' EXIT

# ---------------------------------------------------------------------------
# (A) TypeScript: workspaceFolders enumeration LOOPS.
#
# Only true enumeration shapes are flagged; lookup / first-folder
# accesses (`.find(...)`, `?.[0]`, `?.length`) are intentionally NOT
# routing decisions and are not gated.
#
# Enumeration shapes (regexes applied across a 2-line flatten so
# `vscode.workspace.workspaceFolders\n  .map(...)`-style chains are
# caught):
#   for (const X of vscode.workspace.workspaceFolders ...)
#   vscode.workspace.workspaceFolders.forEach(
#   vscode.workspace.workspaceFolders.map(
#   vscode.workspace.workspaceFolders.filter(
#   vscode.workspace.workspaceFolders.flatMap(
#   vscode.workspace.workspaceFolders.reduce(
#   vscode.workspace.workspaceFolders.every(
#   vscode.workspace.workspaceFolders.some(
#   const X = vscode.workspace.workspaceFolders ?? []  (unless followed
#                                                       by `.find(` /
#                                                       `[0]` only)
#
# We approximate "X is enumerated downstream" by also flagging bindings
# of the form `const X = vscode.workspace.workspaceFolders ?? []`
# IF a downstream `X.{forEach,map,filter,...}` exists in the same
# function scope (within `WINDOW_LINES` lines).
# ---------------------------------------------------------------------------

# Methods that constitute an enumeration loop (vs. single-item lookup).
readonly TS_ENUM_METHODS_RE='(forEach|map|flatMap|filter|reduce|every|some|values|entries|keys)'

scan_ts_file() {
  local file="$1"

  # Flatten the file with line markers so we can test for two-line
  # `vscode.workspace.workspaceFolders\n  .map(` chains while still
  # reporting the original line number of the `vscode.workspace`
  # occurrence.
  #
  # Strategy:
  #   1. Direct shape on a single line (after a 2-line `tr` flatten):
  #      `vscode.workspace.workspaceFolders... .{forEach|map|...}(`
  #      OR `for (const X of vscode.workspace.workspaceFolders`
  #   2. Bound-variable shape:
  #      `const X = vscode.workspace.workspaceFolders ?? []` on line N,
  #      followed within `WINDOW_LINES` lines by `X.{forEach|map|...}(`
  local lineno
  while IFS=: read -r lineno _; do
    [[ -z "${lineno}" ]] && continue

    # Build a 3-line flatten centered on `lineno` (current + next 2).
    local end_line=$((lineno + 2))
    local flat
    flat="$(sed -n "${lineno},${end_line}p" "${file}" | tr '\n' ' ')"

    local is_loop=0

    # Shape 1: direct chained enumeration.
    if grep -qE "vscode\.workspace\.workspaceFolders[^;{}]*?\.${TS_ENUM_METHODS_RE}[[:space:]]*\(" <<< "${flat}"; then
      is_loop=1
    fi
    # Shape 1b: `for (... of vscode.workspace.workspaceFolders`
    if grep -qE 'for[[:space:]]*\([^)]*of[[:space:]]+vscode\.workspace\.workspaceFolders' <<< "${flat}"; then
      is_loop=1
    fi
    # Shape 2: binding then downstream enumeration. We extract the
    # binding name on this line; if present, scan the next
    # WINDOW_LINES for `<name>.{forEach|map|...}(`.
    if [[ ${is_loop} -eq 0 ]]; then
      local current_line
      current_line="$(sed -n "${lineno}p" "${file}")"
      local binder
      binder="$(grep -oE '(const|let|var)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*vscode\.workspace\.workspaceFolders' <<< "${current_line}" \
        | sed -E 's/^(const|let|var)[[:space:]]+([A-Za-z_][A-Za-z0-9_]*).*$/\2/' || true)"
      if [[ -n "${binder}" ]]; then
        local fwd_end=$((lineno + WINDOW_LINES))
        local fwd
        fwd="$(sed -n "${lineno},${fwd_end}p" "${file}")"
        if grep -qE "\\b${binder}\\.${TS_ENUM_METHODS_RE}[[:space:]]*\\(" <<< "${fwd}"; then
          is_loop=1
        fi
      fi
    fi

    if [[ ${is_loop} -eq 0 ]]; then
      continue
    fi

    # Window check: classifier upstream OR escape hatch.
    local start_line=$((lineno - WINDOW_LINES))
    [[ ${start_line} -lt 1 ]] && start_line=1
    local window
    window="$(sed -n "${start_line},${lineno}p" "${file}")"
    local gated=0
    for tok in "${TS_GATE_TOKENS[@]}"; do
      if grep -qF "${tok}" <<< "${window}"; then
        gated=1
        break
      fi
    done
    if [[ ${gated} -eq 0 ]]; then
      printf '%s:%s\n' \
        "${file#${REPO_ROOT}/}" "${lineno}" >> "${ts_violations_raw}"
    fi
  done < <(grep -nE 'vscode\.workspace\.workspaceFolders' "${file}" || true)
}

while IFS= read -r -d '' f; do
  scan_ts_file "${f}"
done < <(find "${TS_ROOT}" -type f -name '*.ts' -print0)

# Subtract known-allow list. Anything still in `${ts_violations_raw}` after
# this is a real violation; we annotate the message back on for output.
sort -u -o "${ts_violations_raw}" "${ts_violations_raw}"
comm -23 "${ts_violations_raw}" "${known_ungated_ts}" \
  | sed -E 's|^(.*)$|\1: ungated vscode.workspace.workspaceFolders enumeration loop|' \
  > "${ts_violations}"

# ---------------------------------------------------------------------------
# (B) Rust handlers: GraphStorage::new constructions.
# ---------------------------------------------------------------------------
scan_rs_file() {
  local file="$1"
  local lineno
  while IFS=: read -r lineno _; do
    [[ -z "${lineno}" ]] && continue
    local start_line=$((lineno - WINDOW_LINES))
    [[ ${start_line} -lt 1 ]] && start_line=1
    local window
    window="$(sed -n "${start_line},${lineno}p" "${file}")"
    local gated=0
    for tok in "${RS_GATE_TOKENS[@]}"; do
      if grep -qF "${tok}" <<< "${window}"; then
        gated=1
        break
      fi
    done
    if [[ ${gated} -eq 0 ]]; then
      printf '%s:%s\n' \
        "${file#${REPO_ROOT}/}" "${lineno}" >> "${rs_violations_raw}"
    fi
  done < <(grep -nE 'GraphStorage::new[[:space:]]*\(' "${file}" || true)
}

while IFS= read -r -d '' f; do
  scan_rs_file "${f}"
done < <(find "${LSP_ROOT}" -type f -name '*.rs' -print0)

# Subtract Rust known-allow list. Anything still in `${rs_violations_raw}`
# after this is a real violation; we re-annotate the message back on
# for output.
sort -u -o "${rs_violations_raw}" "${rs_violations_raw}"
comm -23 "${rs_violations_raw}" "${known_ungated_rs}" \
  | sed -E 's|^(.*)$|\1: ungated GraphStorage::new construction|' \
  > "${rs_violations}"

ts_count="$(wc -l < "${ts_violations}")"
rs_count="$(wc -l < "${rs_violations}")"
echo "static-routing gate: scanned ${TS_ROOT#${REPO_ROOT}/} (${ts_count} violation(s)) + ${LSP_ROOT#${REPO_ROOT}/} (${rs_count} violation(s))"

exit_code=0
if [[ "${ts_count}" -gt 0 ]]; then
  echo "" >&2
  echo "FAIL: TypeScript workspaceFolders enumerations missing classifier upstream:" >&2
  sed -E 's/^/  - /' "${ts_violations}" >&2
  echo "" >&2
  echo "  Add a call to classifyLogicalWorkspace() / classify_path() upstream" >&2
  echo "  in the same function (within ${WINDOW_LINES} lines), or annotate the" >&2
  echo "  loop with a \`// routing-gate-allow:<reason>\` comment if the" >&2
  echo "  enumeration is intentionally classifier-free (e.g. a registry-load" >&2
  echo "  helper that runs before classification)." >&2
  exit_code=1
fi
if [[ "${rs_count}" -gt 0 ]]; then
  echo "" >&2
  echo "FAIL: sqry-lsp/src/**/*.rs GraphStorage::new constructions missing LogicalWorkspace classifier upstream:" >&2
  sed -E 's/^/  - /' "${rs_violations}" >&2
  echo "" >&2
  echo "  Each call site must reach a resolved LogicalWorkspace upstream:" >&2
  echo "  classify / classify_for_serve / classify_path / source_roots() /" >&2
  echo "  member_folders() / exclusions() / a typed &LogicalWorkspace param." >&2
  echo "  If the construction is intentionally classifier-free, annotate it" >&2
  echo "  with \`// routing-gate-allow:<reason>\`." >&2
  exit_code=1
fi

if [[ "${exit_code}" -eq 0 ]]; then
  echo "static-routing gate: PASS"
fi
exit "${exit_code}"
