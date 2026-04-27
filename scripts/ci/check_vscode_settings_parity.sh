#!/usr/bin/env bash
# scripts/ci/check_vscode_settings_parity.sh
#
# STEP_11 settings-parity gate: enforces a 1:1 mapping between
# `sqry-vscode/package.json` `contributes.configuration` keys and the
# settings actually consumed by `vscode.workspace.getConfiguration("sqry").get(<key>)`
# call sites in `sqry-vscode/src/**/*.ts`.
#
# Exit codes:
#   0 — every declared setting is consumed AND every consumed key is declared.
#   1 — one or more parity violations (declared-but-not-consumed OR
#       consumed-but-not-declared). Detail is printed to stderr.
#   2 — invocation / environment error (missing files, missing tools).
#
# Why this gate exists (codex iter1 MAJOR):
#   - The workspaceFolderExcludes dead-code regression that motivated this
#     workstream slipped through review because the package.json key was
#     declared but no `getConfiguration("sqry").get("workspaceFolderExcludes")`
#     call site consumed it. Conversely, missing-projectRootMode-declaration
#     bugs (a `get("projectRootMode")` call with no package.json entry)
#     produce silent fallbacks at runtime. Both classes are caught here.
#
# Implementation notes:
#   - Pure POSIX-ish bash + grep/sed/sort/comm. No node, no jq.
#     The package.json layout for sqry-vscode keeps every property key
#     on its own line as `"sqry.<dotted.key>": {`, which we exploit
#     with a single grep+sed.
#   - Setting names are normalized by stripping the `sqry.` prefix (so
#     declared `sqry.codeLens.enabled` matches `get("codeLens.enabled")`).
#   - Calls inside comments (lines whose first non-space is `*` or `//`)
#     are skipped to avoid documentation false-positives.
#   - The script is intentionally tolerant of multi-line `getConfiguration`
#     chains (see `diagnosticsProvider.ts` line 228) by joining each
#     `.ts` file into a single logical line for the call-site scan.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PKG_JSON="${REPO_ROOT}/sqry-vscode/package.json"
SRC_DIR="${REPO_ROOT}/sqry-vscode/src"

if [[ ! -f "${PKG_JSON}" ]]; then
  echo "error: ${PKG_JSON} not found" >&2
  exit 2
fi
if [[ ! -d "${SRC_DIR}" ]]; then
  echo "error: ${SRC_DIR} not found" >&2
  exit 2
fi

# ---------------------------------------------------------------------------
# 1. Declared keys: every `"sqry.<key>": {` inside the
#    `contributes.configuration.properties` block of package.json.
#    We restrict to lines that look like property declarations so we don't
#    pick up command IDs (which also start with `"sqry.`).
# ---------------------------------------------------------------------------
declared_file="$(mktemp)"
trap 'rm -f "${declared_file}" "${consumed_file}" "${declared_only}" "${consumed_only}" "${consumed_only_raw}" "${known_consumed_no_decl}"' EXIT

# A property declaration line in package.json looks exactly like:
#   <indent>"sqry.something.dotted": {
# where the trailing `: {` distinguishes it from command IDs (which appear
# as `"command": "sqry.something"` and end with a `,` or `"`).
grep -E '^[[:space:]]*"sqry\.[a-zA-Z0-9_.]+"[[:space:]]*:[[:space:]]*\{[[:space:]]*$' "${PKG_JSON}" \
  | sed -E 's/^[[:space:]]*"sqry\.([a-zA-Z0-9_.]+)".*$/\1/' \
  | sort -u > "${declared_file}"

declared_count="$(wc -l < "${declared_file}")"

# ---------------------------------------------------------------------------
# 2. Consumed keys: `getConfiguration("sqry")...get<...>("<key>", ...)`.
#    Three call shapes are accepted:
#
#    (a) Same-line inline:
#        vscode.workspace.getConfiguration("sqry").get<bool>("hover.enabled")
#
#    (b) Multi-line chain (still one expression):
#        vscode.workspace
#          .getConfiguration("sqry")
#          .get<string>("codeLens.segments", [...])
#
#    (c) Bound-variable indirection (the dominant pattern in config.ts):
#        const config = vscode.workspace.getConfiguration("sqry");
#        config.get<string>("path", DEFAULT_BINARY);
#
#    For (a) and (b) we flatten the file with `tr` and run a regex over
#    the resulting one-line text. For (c) we first scan the file for
#    `(const|let|var) <name> = <chain that contains getConfiguration("sqry"
#    | getConfiguration(<S>))>` where `<S>` resolves to the string
#    literal "sqry" (we look for `const SECTION = "sqry"`-style binders),
#    then collect every `<name>.get(...)` call in the same file.
# ---------------------------------------------------------------------------
consumed_file="$(mktemp)"

# Per-file scanner. Argument: the .ts file path.
extract_consumed_for_file() {
  local tsfile="$1"

  # Drop pure-comment lines to avoid docstring examples polluting the
  # binder / call regexes.
  local stripped
  stripped="$(awk '
    /^[[:space:]]*\*/ { next }       # JSDoc continuation
    /^[[:space:]]*\/\// { next }     # // line comment
    { print }
  ' "${tsfile}")"

  # 2a/2b — direct chain:  getConfiguration("sqry") ... .get<...>("<key>")
  printf '%s' "${stripped}" \
    | tr '\n' ' ' \
    | grep -oE 'getConfiguration\("sqry"\)[^;{}]*?\.get(<[^>]+>)?\("[a-zA-Z0-9_.]+"' \
    | grep -oE '\.get(<[^>]+>)?\("[a-zA-Z0-9_.]+"' \
    | sed -E 's/^\.get(<[^>]+>)?\("([a-zA-Z0-9_.]+)".*$/\2/' \
    || true

  # 2c — bound-variable indirection. We collect the set of identifiers
  # in this file that are bound to a `getConfiguration("sqry")` or
  # `getConfiguration(<NAME>)` chain where `<NAME>` is a constant whose
  # value is the literal string "sqry" elsewhere in the same file.
  #
  # First: build the set of constant names whose value is "sqry".
  local sqry_consts
  sqry_consts="$(printf '%s' "${stripped}" \
    | grep -oE '(const|let|var)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*"sqry"' \
    | sed -E 's/^(const|let|var)[[:space:]]+([A-Za-z_][A-Za-z0-9_]*).*$/\2/' \
    | sort -u || true)"

  # Build a regex alternation of: literal `"sqry"` plus each const name.
  local arg_alt='"sqry"'
  if [[ -n "${sqry_consts}" ]]; then
    while IFS= read -r cname; do
      [[ -z "${cname}" ]] && continue
      arg_alt+="|${cname}"
    done <<< "${sqry_consts}"
  fi

  # Now find variable bindings whose RHS contains
  # `getConfiguration(<arg_alt>)`. The RHS may span multiple lines, so
  # we flatten first and use a non-greedy match.
  local binders
  binders="$(printf '%s' "${stripped}" \
    | tr '\n' ' ' \
    | grep -oE "(const|let|var)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[^;]*getConfiguration\\((${arg_alt})\\)" \
    | sed -E 's/^(const|let|var)[[:space:]]+([A-Za-z_][A-Za-z0-9_]*).*$/\2/' \
    | sort -u || true)"

  if [[ -n "${binders}" ]]; then
    # For each binder name, emit every `<name>.get<...>("<key>")` we see
    # in this file's stripped contents. The generic + first string arg
    # may span lines (see the union-typed `autoIndexOnOpen` call in
    # config.ts), so flatten with `tr` first. Note the `[^>]*` inside the
    # generic is intentionally tolerant of embedded quoted unions like
    # `<"always" | "prompt" | "never">`.
    local flat
    flat="$(printf '%s' "${stripped}" | tr '\n' ' ')"
    while IFS= read -r bname; do
      [[ -z "${bname}" ]] && continue
      printf '%s' "${flat}" \
        | grep -oE "\\b${bname}\\.get(<[^>]+>)?[[:space:]]*\\([[:space:]]*\"[a-zA-Z0-9_.]+\"" \
        | sed -E "s/^${bname}\\.get(<[^>]+>)?[[:space:]]*\\([[:space:]]*\"([a-zA-Z0-9_.]+)\".*$/\\2/" \
        || true
    done <<< "${binders}"
  fi
}

while IFS= read -r -d '' tsfile; do
  extract_consumed_for_file "${tsfile}" >> "${consumed_file}" || true
done < <(find "${SRC_DIR}" -type f -name '*.ts' -print0)

# Deduplicate.
sort -u -o "${consumed_file}" "${consumed_file}"
consumed_count="$(wc -l < "${consumed_file}")"

# ---------------------------------------------------------------------------
# 3. Diff. `comm` is the minimal POSIX tool for this.
# ---------------------------------------------------------------------------
# ---------------------------------------------------------------------------
# 3a. Known pre-existing parity gaps. Each entry is a `sqry.<key>` (without
#     the `sqry.` prefix) that is currently outside parity AND has a
#     scoped-out follow-up. This list is INTENTIONALLY narrow — every
#     entry must have a tracked owner. Adding an entry without an owner
#     defeats the gate.
#
#     - allowInsecureDownload: consumed in binaryDownloader.ts since
#       commit c89293e9f9f0 ("fix(vscode): harden binary autodownload
#       verification"); never declared in package.json. This is a
#       pre-existing extension bug, not a workspace-aware-cross-repo
#       regression. Tracked for STEP_5_VSCODE_EXTENSION follow-up; this
#       gate refuses to be the forcing function for unrelated declaration
#       work, but it WILL refuse to allow new entries to grow this list.
# ---------------------------------------------------------------------------
known_consumed_no_decl="$(mktemp)"
cat > "${known_consumed_no_decl}" <<'EOF'
allowInsecureDownload
EOF
sort -u -o "${known_consumed_no_decl}" "${known_consumed_no_decl}"

declared_only="$(mktemp)"
consumed_only_raw="$(mktemp)"
consumed_only="$(mktemp)"
comm -23 "${declared_file}" "${consumed_file}" > "${declared_only}"
comm -13 "${declared_file}" "${consumed_file}" > "${consumed_only_raw}"
comm -23 "${consumed_only_raw}" "${known_consumed_no_decl}" > "${consumed_only}"

declared_only_count="$(wc -l < "${declared_only}")"
consumed_only_count="$(wc -l < "${consumed_only}")"

echo "settings-parity gate: ${declared_count} declared, ${consumed_count} consumed"

exit_code=0
if [[ "${declared_only_count}" -gt 0 ]]; then
  echo "" >&2
  echo "FAIL: declared in package.json but never consumed in src/**/*.ts:" >&2
  sed -E 's/^/  - sqry./' "${declared_only}" >&2
  exit_code=1
fi
if [[ "${consumed_only_count}" -gt 0 ]]; then
  echo "" >&2
  echo "FAIL: consumed in src/**/*.ts but never declared in package.json:" >&2
  sed -E 's/^/  - sqry./' "${consumed_only}" >&2
  exit_code=1
fi

if [[ "${exit_code}" -eq 0 ]]; then
  echo "settings-parity gate: PASS"
fi

exit "${exit_code}"
