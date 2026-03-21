#!/usr/bin/env bash
# Quick helper script for sqry queries
# This makes it easy to use sqry for code search without MCP complexity

set -eu

SQRY_BIN="${SQRY_BIN:-sqry}"
PROJECT_PATH="${1:-.}"
QUERY="${2:-kind:function}"

usage() {
    cat << EOF
Usage: $0 [PATH] [QUERY]

Quick sqry query helper.

Arguments:
  PATH    Project directory (default: current directory)
  QUERY   Query expression (default: "kind:function")

Examples:
  $0 . "kind:function AND name~=/search/"
  $0 /path/to/project "kind:class"
  $0 . "kind:function AND async:true"

Query Syntax:
  kind:TYPE              - Filter by symbol type (function, class, struct, etc.)
  name:NAME              - Filter by exact name
  name~=/REGEX/          - Filter by regex pattern
  async:true             - Filter async functions
  visibility:public      - Filter by visibility

  Combine with AND, OR, NOT:
  kind:function AND name~=/test/
  kind:class OR kind:struct
  kind:method NOT name~=/private/

Output:
  Returns JSON with symbol information (name, file, line, metadata)
EOF
    exit 1
}

if [[ "${1:-}" == "-h" ]] || [[ "${1:-}" == "--help" ]]; then
    usage
fi

# Run query and format output
"$SQRY_BIN" query "$QUERY" --json "$PROJECT_PATH" | jq -c '.results[] | {
  name: .name,
  type: .symbol_type,
  file: .file_path,
  line: .start_line,
  visibility: .metadata.visibility,
  async: .metadata.async
}'
