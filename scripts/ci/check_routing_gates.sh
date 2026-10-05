#!/usr/bin/env bash
# scripts/ci/check_routing_gates.sh
#
# STEP_11 static-routing gate: a structural barrier against reintroducing the
# regression class that motivated the workspace-aware-cross-repo design.
# Three patterns are forbidden:
#
#   (A) `vscode.workspace.workspaceFolders` enumeration loops in
#       `sqry-vscode/src/**/*.ts` with no classifier call (a TS gate token,
#       below) in code within the preceding window. The original
#       `aivcs-runner-folder` regression slipped in as exactly this shape: a
#       `for (const folder of vscode.workspace.workspaceFolders)` loop
#       running per-folder filesystem probes without the LogicalWorkspace
#       classifier gating.
#
#   (B) `GraphStorage::new(...)` constructions anywhere in
#       `sqry-lsp/src/**/*.rs` with no LogicalWorkspace touchpoint (a Rust
#       gate token, below) in code within the preceding window. LSP code must
#       consult the LogicalWorkspace before opening per-folder graph storage.
#
#   (C) `vscode.open` / `vscode.openWith` navigation named by a string
#       literal in any quote form (single quotes, double quotes, or a
#       template literal with no substitution) anywhere in
#       `sqry-vscode/src/**/*.ts` outside `workspaceGuard.ts`. Result paths
#       come from indexed data, so opening one directly can reach a file
#       outside the workspace via an absolute path, a `..` sequence, or a
#       symlink. Every result-driven open goes through
#       `openFileWithinWorkspace` / the `sqry.openResultFile` command, which
#       confine and canonicalize first.
#
#       Not gated: `vscode.window.showTextDocument`. Its call sites open an
#       in-memory buffer (`openTextDocument({content})`) and the user's own
#       `.code-workspace` file, neither of which is result-derived. A future
#       result-driven `showTextDocument` should route through the guard like
#       everything else.
#
#       The literal scan is a guardrail against the accidental
#       reintroduction that review would otherwise have to catch by eye.
#       It is not a sandbox: a command name assembled at runtime, or a
#       future navigation command nobody has thought of, passes it. That
#       is the same bargain patterns (A) and (B) make.
#
# Every decision is taken on code, never on comments or string literals
# (surface parity W4 round 3, design W4-D21). Before any scan, one python3
# lexer (the quoted heredoc below) reads every scanned file and writes three
# artefacts per file into this run's temporary directory:
#
#   - a code view: the file with every comment (delimiters included) and the
#     contents of every string, character, template and regular-expression
#     literal replaced by spaces, literal delimiters and newlines kept, so
#     every line number is the file's own;
#   - a line-comment table: the line number and the text of each `//`
#     comment;
#   - for TypeScript, a string table: the start line and the content of each
#     single-quoted, double-quoted and substitution-free template literal.
#
# Occurrences (a construction, a loop, a `workspaceFolders` mention) and
# gate tokens are searched in the code view, so a comment or a string that
# names either is neither. Family C reads the string table.
#
# The Rust lexer knows `//` comments (doc comments included), `/* */`
# comments nested to any depth, string literals with escapes and `b` / `c`
# prefixes, raw strings with any number of `#` and `r` / `br` / `cr`
# prefixes (recognised only where the prefix does not continue an
# identifier, so `r#ident` stays code), and character literals: a `'`
# followed by `\`, or by one character and then a `'`, opens one, and any
# other `'` is a lifetime or a label. The TypeScript lexer knows `//` and
# `/* */` comments, single and double quoted strings with escapes, template
# literals with `${ }` substitutions nested to any depth (a substitution is
# code), and regular-expression literals, which start at a `/` not followed
# by `/` or `*` when the previous significant token is not an identifier
# (other than return, typeof, instanceof, in, of, new, delete, void, throw,
# case, do, else, yield, await), not a number, not `)` or `]`, and not the
# end of a literal; a character class `[...]` inside one is honoured.
#
# An intentionally classifier-free site carries an annotation: a line
# comment whose text, after the leading `//`, any further `/` or `!`, and
# whitespace, starts with `routing-gate-allow:` followed by at least one
# non-whitespace character (the reason), for example
# `// routing-gate-allow: registry load runs before classification`. It is
# read from the line-comment table only: the same text in a string, in a
# block comment, or with nothing after the colon is not an annotation.
# Families A and B accept it anywhere in the window; family C on the same or
# the preceding line. The annotation is explicit on purpose, so reviewers
# see every intentional bypass.
#
# `ROUTING_GATE_TRACE=1` prints, on stdout, one tab-separated line per
# occurrence: `routing-gate-trace`, the family (A, B or C), the
# repository-relative path, the line, and what decided it: `token <token>`,
# `annotation`, `allowlist`, or `violation`. A family A mention that is not
# an enumeration loop is `not an enumeration`; a family C literal in the
# guard file is `guard file`. Without the variable the output is unchanged.
#
# What a pass does not show: the window stays a window. A gate token in code
# anywhere in the preceding lines accepts a construction, including an
# identifier that merely contains a token and a token in another function
# (design W4-D23).
#
# Exit codes:
#   0: every loop, construction and navigation is gated, annotated or
#      allowlisted.
#   1: one or more violations.
#   2: invocation or environment error: a missing source root, no python3 on
#      PATH, or a file the lexer cannot lex (it ends inside a comment or a
#      literal, or a regular expression is cut by a newline), named on
#      stderr as `error: cannot lex <path>: <state>`.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TS_ROOT="${REPO_ROOT}/sqry-vscode/src"
# All of `sqry-lsp/src/` is scanned, not just `handlers/`: non-handler
# `GraphStorage::new(...)` sites in `server.rs` and `session.rs` are covered.
# The DAG text references `sqry-lsp/src/handlers/**/*.rs`, but the spirit of
# the gate is "no new ungated GraphStorage::new in the LSP crate";
# restricting it to one subdirectory creates an obvious bypass. Codex iter1
# MAJOR.
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
if ! command -v python3 >/dev/null 2>&1; then
  echo "error: python3 not found on PATH; the static-routing gate's lexer needs it" >&2
  exit 2
fi

# Tokens that legitimately gate a workspaceFolders enumeration. Any of
# these in code in the preceding window absolves the loop.
readonly -a TS_GATE_TOKENS=(
  "classifyLogicalWorkspace"
  "classify_path"
  "classifyPath"
  "nonExcludedFolders"
  "enumerateClassifiedFolders"
  "buildWorkspaceInitializationPayload"
  "isFolderExcluded"
)

# Tokens that legitimately gate a GraphStorage::new construction: direct
# LogicalWorkspace touchpoints, symbols and methods that can ONLY be reached
# through a resolved `LogicalWorkspace` (codex iter1 MAJOR tightening:
# `resolve_path(` and `workspace_root` were too broad, matching unrelated
# function-name fragments):
#
#   - `LogicalWorkspace::classify`: the classifier itself
#   - `classify_for_serve`: the daemon classifier wrapper
#   - `classify_path`: the SessionManager re-export
#   - `session.classify`: the same, dotted form
#   - `logical_workspace`: the Session / Engine accessor
#   - `LogicalWorkspace`: the type name; a function that takes
#     `&LogicalWorkspace` has a workspace by construction
#   - `.source_roots()`, `.member_folders()`, `.exclusions()`: only callable
#     on a resolved workspace
#
# The annotation is not a token: it is read from the line-comment table.
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
)

WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT

known_ungated_ts="${WORK}/known_ungated_ts"
cat > "${known_ungated_ts}" <<'EOF'
EOF
sort -u -o "${known_ungated_ts}" "${known_ungated_ts}"

# Rust LSP handler and session sites that pre-date the
# workspace-aware-cross-repo workstream, keyed by repository-relative path
# and line. Each entry must carry an explicit owner; STEP_11_4_CROSS_CUTTING
# (`[units.STEP_11_4_CROSS_CUTTING]` in the DAG TOML) is the unit that
# converts these to classifier-gated implementations, and its acceptance
# contract explicitly says "All 6 LSP handler types ... consult
# LogicalWorkspace.classify() before any filesystem probe".
#
# Format (per line): <repo-relative-path>:<lineno>
#
#   - the graph_stats, index, trace_path and is_node_in_cycle handlers: each
#     resolves a workspace-relative path through
#     SessionManager::resolve_path before constructing GraphStorage.
#     SessionManager::resolve_path enforces workspace-bound
#     canonicalization (rejects directory traversal), which is the de-facto
#     "classifier upstream" today; STEP_11_4_CROSS_CUTTING upgrades these to
#     explicit LogicalWorkspace::classify() gates per the DAG. Tracked there.
#
#   - SessionManager::graph() in session.rs loads the session-level graph
#     from `current_index_root`, which is itself derived from the
#     SessionManager's `index_root` config (workspace-bound). Tracked under
#     STEP_11_4_CROSS_CUTTING for an explicit classifier touchpoint.
known_ungated_rs="${WORK}/known_ungated_rs"
cat > "${known_ungated_rs}" <<'EOF'
sqry-lsp/src/handlers/graph_stats.rs:30
sqry-lsp/src/handlers/index.rs:780
sqry-lsp/src/handlers/trace_path.rs:449
sqry-lsp/src/handlers/is_node_in_cycle.rs:223
sqry-lsp/src/session.rs:524
EOF
sort -u -o "${known_ungated_rs}" "${known_ungated_rs}"

ts_violations_raw="${WORK}/ts_violations_raw"
ts_violations="${WORK}/ts_violations"
ts_nav_violations="${WORK}/ts_nav_violations"
rs_violations_raw="${WORK}/rs_violations_raw"
rs_violations="${WORK}/rs_violations"
: > "${ts_violations_raw}"
: > "${ts_nav_violations}"
: > "${rs_violations_raw}"

# ---------------------------------------------------------------------------
# The lexer: one python3 run over every file the scans below read.
# ---------------------------------------------------------------------------
find "${TS_ROOT}" -type f -name '*.ts' -print0 > "${WORK}/ts.files"
find "${LSP_ROOT}" -type f -name '*.rs' -print0 > "${WORK}/rs.files"

if ! python3 - "${REPO_ROOT}" "${WORK}" <<'PY'
import os
import sys

REPO_ROOT, WORK = sys.argv[1], sys.argv[2]

REGEX_KEYWORDS = {
    "return", "typeof", "instanceof", "in", "of", "new", "delete", "void",
    "throw", "case", "do", "else", "yield", "await",
}


class LexError(Exception):
    """The file ends inside a comment or a literal; the argument names where."""


def blank(text):
    """Every character but a newline becomes a space."""
    return "".join("\n" if ch == "\n" else " " for ch in text)


def ident_char(ch):
    return ch.isalnum() or ch == "_"


def lex_rust(text):
    """(code view, [(line, comment text)])."""
    out = []
    comments = []
    n = len(text)
    i = 0
    line = 1
    while i < n:
        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if c == "/" and nxt == "/":
            end = text.find("\n", i)
            if end == -1:
                end = n
            comments.append((line, text[i:end]))
            out.append(blank(text[i:end]))
            i = end
            continue
        if c == "/" and nxt == "*":
            depth = 1
            j = i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth += 1
                    j += 2
                elif text.startswith("*/", j):
                    depth -= 1
                    j += 2
                else:
                    j += 1
            if depth:
                raise LexError("inside a block comment")
            out.append(blank(text[i:j]))
            line += text.count("\n", i, j)
            i = j
            continue
        if c in "rbc" and (i == 0 or not ident_char(text[i - 1])):
            k = i + 1 if c in "bc" and nxt == "r" else i
            if text[k] == "r":
                h = k + 1
                while h < n and text[h] == "#":
                    h += 1
                if h < n and text[h] == '"':
                    close = '"' + "#" * (h - k - 1)
                    end = text.find(close, h + 1)
                    if end == -1:
                        raise LexError("inside a raw string literal")
                    out.append(text[i:h + 1])
                    out.append(blank(text[h + 1:end]))
                    out.append(close)
                    line += text.count("\n", i, end + len(close))
                    i = end + len(close)
                    continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            if j >= n:
                raise LexError("inside a string literal")
            out.append('"' + blank(text[i + 1:j]) + '"')
            line += text.count("\n", i, j)
            i = j + 1
            continue
        if c == "'":
            if nxt == "\\":
                j = i + 3
                while j < n and text[j] not in "'\n":
                    j += 1
                if j >= n or text[j] != "'":
                    raise LexError("inside a character literal")
            elif nxt not in ("", "\n") and i + 2 < n and text[i + 2] == "'":
                j = i + 2
            else:
                out.append(c)
                i += 1
                continue
            out.append("'" + blank(text[i + 1:j]) + "'")
            i = j + 1
            continue
        if c == "\n":
            line += 1
        out.append(c)
        i += 1
    return "".join(out), comments, []


def lex_typescript(text):
    """(code view, [(line, comment text)], [(line, string content)])."""
    out = []
    comments = []
    strings = []
    n = len(text)
    i = 0
    line = 1
    previous = ("start", "")
    # "brace" for a `{` in code, ("template", start line) for a `${`.
    stack = []
    template = None  # (start line, has a substitution) while in template text

    while i < n:
        if template is not None:
            start_line, substituted = template
            j = i
            how = None
            while j < n:
                ch = text[j]
                if ch == "\\":
                    j += 2
                    continue
                if ch == "`":
                    how = "close"
                    break
                if ch == "$" and j + 1 < n and text[j + 1] == "{":
                    how = "substitution"
                    break
                j += 1
            if how is None:
                raise LexError("inside a template literal")
            body = text[i:j]
            out.append(blank(body))
            line += body.count("\n")
            if how == "close":
                if not substituted:
                    strings.append((start_line, body))
                out.append("`")
                i = j + 1
                template = None
                previous = ("literal", "")
            else:
                out.append("${")
                i = j + 2
                stack.append(("template", start_line))
                template = None
                previous = ("punct", "{")
            continue

        c = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        if c == "\n":
            line += 1
            out.append(c)
            i += 1
            continue
        if c.isspace():
            out.append(c)
            i += 1
            continue
        if c == "/" and nxt == "/":
            end = text.find("\n", i)
            if end == -1:
                end = n
            comments.append((line, text[i:end]))
            out.append(blank(text[i:end]))
            i = end
            continue
        if c == "/" and nxt == "*":
            end = text.find("*/", i + 2)
            if end == -1:
                raise LexError("inside a block comment")
            out.append(blank(text[i:end + 2]))
            line += text.count("\n", i, end + 2)
            i = end + 2
            continue
        if c in "'\"":
            j = i + 1
            while j < n and text[j] != c:
                if text[j] == "\\":
                    j += 3 if text.startswith("\r\n", j + 1) else 2
                    continue
                if text[j] == "\n":
                    raise LexError("inside a string literal")
                j += 1
            if j >= n:
                raise LexError("inside a string literal")
            strings.append((line, text[i + 1:j]))
            out.append(c + blank(text[i + 1:j]) + c)
            line += text.count("\n", i, j)
            i = j + 1
            previous = ("literal", "")
            continue
        if c == "`":
            out.append("`")
            template = (line, False)
            i += 1
            continue
        if c == "/":
            regex_allowed = (
                previous[0] == "start"
                or (previous[0] == "punct" and previous[1] not in (")", "]"))
                or (previous[0] == "ident" and previous[1] in REGEX_KEYWORDS)
            )
            if regex_allowed:
                j = i + 1
                in_class = False
                while True:
                    if j >= n:
                        raise LexError("inside a regular expression literal")
                    ch = text[j]
                    if ch == "\n":
                        raise LexError("a regular expression interrupted by a newline")
                    if ch == "\\":
                        if j + 1 < n and text[j + 1] == "\n":
                            raise LexError("a regular expression interrupted by a newline")
                        j += 2
                        continue
                    if in_class:
                        if ch == "]":
                            in_class = False
                    elif ch == "[":
                        in_class = True
                    elif ch == "/":
                        break
                    j += 1
                out.append("/" + blank(text[i + 1:j]) + "/")
                i = j + 1
                previous = ("literal", "")
                continue
            out.append(c)
            i += 1
            previous = ("punct", c)
            continue
        if c.isalpha() or c in "_$":
            j = i + 1
            while j < n and (text[j].isalnum() or text[j] in "_$"):
                j += 1
            word = text[i:j]
            out.append(word)
            i = j
            previous = ("ident", word)
            continue
        if c.isdigit():
            j = i + 1
            while j < n and (text[j].isalnum() or text[j] in "_."):
                j += 1
            out.append(text[i:j])
            i = j
            previous = ("number", "")
            continue
        if c == "{":
            stack.append("brace")
        elif c == "}" and stack:
            top = stack.pop()
            if top != "brace":
                out.append("}")
                i += 1
                template = (top[1], True)
                continue
        out.append(c)
        i += 1
        previous = ("punct", c)

    if template is not None or any(entry != "brace" for entry in stack):
        raise LexError("inside a template literal")
    return "".join(out), comments, strings


def table_text(text):
    """A comment's text for its table: tabs and carriage returns as spaces."""
    return text.replace("\t", " ").replace("\r", " ")


def string_cell(text):
    """A string's content for its table, escaped so it stays on one line."""
    return (text.replace("\\", "\\\\").replace("\t", "\\t")
            .replace("\r", "\\r").replace("\n", "\\n"))


def write(path, content):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", errors="surrogateescape", newline="") as handle:
        handle.write(content)


for listing, lexer in (("rs.files", lex_rust), ("ts.files", lex_typescript)):
    with open(os.path.join(WORK, listing), "rb") as handle:
        paths = [p.decode("utf-8", "surrogateescape") for p in handle.read().split(b"\0") if p]
    for path in sorted(paths):
        rel = os.path.relpath(path, REPO_ROOT)
        with open(path, encoding="utf-8", errors="surrogateescape", newline="") as handle:
            text = handle.read()
        try:
            code, comments, strings = lexer(text)
        except LexError as error:
            print(f"error: cannot lex {rel}: {error}", file=sys.stderr)
            sys.exit(2)
        if code.count("\n") != text.count("\n") or len(code) != len(text):
            print(f"error: cannot lex {rel}: the code view changed the file's shape", file=sys.stderr)
            sys.exit(2)
        write(os.path.join(WORK, "code", rel), code)
        write(os.path.join(WORK, "comments", rel + ".tsv"),
              "".join(f"{number}\t{table_text(body)}\n" for number, body in comments))
        if lexer is lex_typescript:
            write(os.path.join(WORK, "strings", rel + ".tsv"),
                  "".join(f"{number}\t{string_cell(body)}\n" for number, body in strings))
PY
then
  exit 2
fi

code_view() {
  printf '%s/code/%s' "${WORK}" "${1#"${REPO_ROOT}"/}"
}

comment_table() {
  printf '%s/comments/%s.tsv' "${WORK}" "${1#"${REPO_ROOT}"/}"
}

string_table() {
  printf '%s/strings/%s.tsv' "${WORK}" "${1#"${REPO_ROOT}"/}"
}

# trace <family> <path> <line> <decision>
trace() {
  if [[ "${ROUTING_GATE_TRACE:-}" == "1" ]]; then
    printf 'routing-gate-trace\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4"
  fi
}

# annotation_in_range <comment table> <first line> <last line>: exit 0 when a
# line comment in the range is an annotation with a non-empty reason.
annotation_in_range() {
  awk -F'\t' -v first="$2" -v last="$3" '
    ($1 + 0) >= (first + 0) && ($1 + 0) <= (last + 0) {
      text = $2
      sub(/^\/\/[\/!]*[[:space:]]*/, "", text)
      if (text ~ /^routing-gate-allow:[[:space:]]*[^[:space:]]/) {
        found = 1
      }
    }
    END { exit found ? 0 : 1 }
  ' "$1"
}

# window_token <code view> <first line> <last line> <token...>: prints
# `token <token>` for the first token, in list order, found in the code view
# between the two lines; exits 1 when none is.
window_token() {
  local code="$1" first="$2" last="$3"
  shift 3
  local window
  window="$(sed -n "${first},${last}p" "${code}")"
  local tok
  for tok in "$@"; do
    if grep -qF -- "${tok}" <<< "${window}"; then
      printf 'token %s' "${tok}"
      return 0
    fi
  done
  return 1
}

# ---------------------------------------------------------------------------
# (A) TypeScript: workspaceFolders enumeration LOOPS.
#
# Only true enumeration shapes are flagged; lookup / first-folder
# accesses (`.find(...)`, `?.[0]`, `?.length`) are intentionally NOT
# routing decisions and are not gated.
#
# Enumeration shapes (regexes applied across a 3-line flatten of the code
# view so `vscode.workspace.workspaceFolders\n  .map(...)`-style chains are
# caught):
#   for (const X of vscode.workspace.workspaceFolders ...)
#   vscode.workspace.workspaceFolders.forEach(
#   vscode.workspace.workspaceFolders.map(
#   vscode.workspace.workspaceFolders.filter(
#   vscode.workspace.workspaceFolders.flatMap(
#   vscode.workspace.workspaceFolders.reduce(
#   vscode.workspace.workspaceFolders.every(
#   vscode.workspace.workspaceFolders.some(
#   const X = vscode.workspace.workspaceFolders ?? []  (when a downstream
#       `X.{forEach,map,filter,...}(` exists within `WINDOW_LINES` lines)
# ---------------------------------------------------------------------------

# Methods that constitute an enumeration loop (vs. single-item lookup).
readonly TS_ENUM_METHODS_RE='(forEach|map|flatMap|filter|reduce|every|some|values|entries|keys)'

scan_ts_file() {
  local file="$1"
  local rel="${file#"${REPO_ROOT}"/}"
  local code comments
  code="$(code_view "${file}")"
  comments="$(comment_table "${file}")"

  local lineno
  while IFS=: read -r lineno _; do
    [[ -z "${lineno}" ]] && continue

    # A 3-line flatten centered on `lineno` (current + next 2).
    local end_line=$((lineno + 2))
    local flat
    flat="$(sed -n "${lineno},${end_line}p" "${code}" | tr '\n' ' ')"

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
      current_line="$(sed -n "${lineno}p" "${code}")"
      local binder
      binder="$(grep -oE '(const|let|var)[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*=[[:space:]]*vscode\.workspace\.workspaceFolders' <<< "${current_line}" \
        | sed -E 's/^(const|let|var)[[:space:]]+([A-Za-z_][A-Za-z0-9_]*).*$/\2/' || true)"
      if [[ -n "${binder}" ]]; then
        local fwd_end=$((lineno + WINDOW_LINES))
        local fwd
        fwd="$(sed -n "${lineno},${fwd_end}p" "${code}")"
        if grep -qE "\\b${binder}\\.${TS_ENUM_METHODS_RE}[[:space:]]*\\(" <<< "${fwd}"; then
          is_loop=1
        fi
      fi
    fi

    if [[ ${is_loop} -eq 0 ]]; then
      trace A "${rel}" "${lineno}" "not an enumeration"
      continue
    fi

    # Window check: a classifier in code upstream, or an annotation.
    local start_line=$((lineno - WINDOW_LINES))
    [[ ${start_line} -lt 1 ]] && start_line=1
    local decision=""
    decision="$(window_token "${code}" "${start_line}" "${lineno}" "${TS_GATE_TOKENS[@]}" || true)"
    if [[ -z "${decision}" ]] && annotation_in_range "${comments}" "${start_line}" "${lineno}"; then
      decision="annotation"
    fi
    if [[ -z "${decision}" ]]; then
      printf '%s:%s\n' "${rel}" "${lineno}" >> "${ts_violations_raw}"
      if grep -qxF -- "${rel}:${lineno}" "${known_ungated_ts}"; then
        decision="allowlist"
      else
        decision="violation"
      fi
    fi
    trace A "${rel}" "${lineno}" "${decision}"
  done < <(grep -nE 'vscode\.workspace\.workspaceFolders' "${code}" || true)
}

while IFS= read -r -d '' f; do
  scan_ts_file "${f}"
done < "${WORK}/ts.files"

# Subtract known-allow list. Anything still in `${ts_violations_raw}` after
# this is a real violation; we annotate the message back on for output.
sort -u -o "${ts_violations_raw}" "${ts_violations_raw}"
comm -23 "${ts_violations_raw}" "${known_ungated_ts}" \
  | sed -E 's|^(.*)$|\1: ungated vscode.workspace.workspaceFolders enumeration loop|' \
  > "${ts_violations}"

# ---------------------------------------------------------------------------
# (C) TypeScript: raw `vscode.open` navigation outside the guard.
#
# `workspaceGuard.ts` is the one place allowed to open a file, because
# it canonicalizes and confines the path first. Everywhere else must
# call `openFileWithinWorkspace` or route a tree item / code action
# through the `sqry.openResultFile` command.
#
# A navigation is a string literal whose content is exactly `vscode.open`
# or `vscode.openWith`, in single quotes, double quotes or a template with
# no substitution. An annotation on the same or the preceding line makes an
# intentional bypass visible in review rather than silent.

scan_ts_nav_file() {
  local file="$1"
  local rel="${file#"${REPO_ROOT}"/}"
  local strings comments
  strings="$(string_table "${file}")"
  comments="$(comment_table "${file}")"
  local guard=0
  # The guard itself is the sanctioned opener.
  if [[ "$(basename "${file}")" == "workspaceGuard.ts" ]]; then
    guard=1
  fi

  local lineno content
  while IFS=$'\t' read -r lineno content; do
    [[ -z "${lineno}" ]] && continue
    if [[ "${content}" != "vscode.open" && "${content}" != "vscode.openWith" ]]; then
      continue
    fi
    if [[ ${guard} -eq 1 ]]; then
      trace C "${rel}" "${lineno}" "guard file"
      continue
    fi
    local previous_line=$((lineno > 1 ? lineno - 1 : 1))
    if annotation_in_range "${comments}" "${previous_line}" "${lineno}"; then
      trace C "${rel}" "${lineno}" "annotation"
      continue
    fi
    printf '%s:%s\n' "${rel}" "${lineno}" >> "${ts_nav_violations}"
    trace C "${rel}" "${lineno}" "violation"
  done < "${strings}"
}

while IFS= read -r -d '' f; do
  scan_ts_nav_file "${f}"
done < "${WORK}/ts.files"

sort -u -o "${ts_nav_violations}" "${ts_nav_violations}"

# ---------------------------------------------------------------------------
# (B) Rust: GraphStorage::new constructions anywhere in sqry-lsp/src.
# ---------------------------------------------------------------------------
scan_rs_file() {
  local file="$1"
  local rel="${file#"${REPO_ROOT}"/}"
  local code comments
  code="$(code_view "${file}")"
  comments="$(comment_table "${file}")"
  local lineno
  while IFS=: read -r lineno _; do
    [[ -z "${lineno}" ]] && continue
    local start_line=$((lineno - WINDOW_LINES))
    [[ ${start_line} -lt 1 ]] && start_line=1
    local decision=""
    decision="$(window_token "${code}" "${start_line}" "${lineno}" "${RS_GATE_TOKENS[@]}" || true)"
    if [[ -z "${decision}" ]] && annotation_in_range "${comments}" "${start_line}" "${lineno}"; then
      decision="annotation"
    fi
    if [[ -z "${decision}" ]]; then
      printf '%s:%s\n' "${rel}" "${lineno}" >> "${rs_violations_raw}"
      if grep -qxF -- "${rel}:${lineno}" "${known_ungated_rs}"; then
        decision="allowlist"
      else
        decision="violation"
      fi
    fi
    trace B "${rel}" "${lineno}" "${decision}"
  done < <(grep -nE 'GraphStorage::new[[:space:]]*\(' "${code}" || true)
}

while IFS= read -r -d '' f; do
  scan_rs_file "${f}"
done < "${WORK}/rs.files"

# Subtract Rust known-allow list. Anything still in `${rs_violations_raw}`
# after this is a real violation; we re-annotate the message back on
# for output.
sort -u -o "${rs_violations_raw}" "${rs_violations_raw}"
comm -23 "${rs_violations_raw}" "${known_ungated_rs}" \
  | sed -E 's|^(.*)$|\1: ungated GraphStorage::new construction|' \
  > "${rs_violations}"

ts_count="$(wc -l < "${ts_violations}")"
ts_nav_count="$(wc -l < "${ts_nav_violations}")"
rs_count="$(wc -l < "${rs_violations}")"
echo "static-routing gate: scanned ${TS_ROOT#"${REPO_ROOT}"/} (${ts_count} enumeration + ${ts_nav_count} navigation violation(s)) + ${LSP_ROOT#"${REPO_ROOT}"/} (${rs_count} violation(s))"

exit_code=0
if [[ "${ts_nav_count}" -gt 0 ]]; then
  echo "" >&2
  echo "FAIL: raw \`vscode.open\` navigation outside sqry-vscode/src/workspaceGuard.ts:" >&2
  sed -E 's/^/  - /' "${ts_nav_violations}" >&2
  echo "" >&2
  echo "  Result paths come from indexed data and can point outside the" >&2
  echo "  workspace. Open them with openFileWithinWorkspace(), or route the" >&2
  echo "  tree item / code action through the \`sqry.openResultFile\` command." >&2
  echo "  If the path is genuinely not result-derived, annotate the call with" >&2
  echo "  a \`// routing-gate-allow: <reason>\` line comment." >&2
  exit_code=1
fi
if [[ "${ts_count}" -gt 0 ]]; then
  echo "" >&2
  echo "FAIL: TypeScript workspaceFolders enumerations missing classifier upstream:" >&2
  sed -E 's/^/  - /' "${ts_violations}" >&2
  echo "" >&2
  echo "  Add a call to classifyLogicalWorkspace() / classify_path() upstream" >&2
  echo "  in the same function (within ${WINDOW_LINES} lines), or annotate the" >&2
  echo "  loop with a \`// routing-gate-allow: <reason>\` line comment if the" >&2
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
  echo "  with a \`// routing-gate-allow: <reason>\` line comment." >&2
  exit_code=1
fi

if [[ "${exit_code}" -eq 0 ]]; then
  echo "static-routing gate: PASS"
fi
exit "${exit_code}"
