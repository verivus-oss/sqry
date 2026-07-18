#!/bin/bash
# Sync version from Cargo.toml [workspace.package] to downstream docs, package
# metadata, and first-party cargo-vet exemptions.
#
# Usage:
#   scripts/sync-versions.sh          # Check mode — exit 1 if any file is stale
#   scripts/sync-versions.sh --fix    # Fix mode — update all files in-place
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$REPO_ROOT"

FIX_MODE=false
if [ "${1:-}" = "--fix" ]; then
    FIX_MODE=true
fi

# --- Source of truth ---
WORKSPACE_VERSION=$(grep -A5 '\[workspace.package\]' Cargo.toml \
    | grep '^version' | head -1 \
    | sed 's/.*= *"\([^"]*\)".*/\1/')
TODAY=$(date +%Y-%m-%d)

echo "Workspace version: $WORKSPACE_VERSION"
echo "Date: $TODAY"
echo ""

ERRORS=0
WARNINGS=0
FIXED=0

# --- Parse release-manifest.toml for public directories ---
# Uses the same Python/tomllib approach as sanitize-for-oss.sh
MANIFEST="${REPO_ROOT}/release-manifest.toml"

manifest_array() {
  local key="$1"
  python3 -c "
import tomllib
with open('${MANIFEST}', 'rb') as f:
    data = tomllib.load(f)
keys = '${key}'.split('.')
obj = data
for k in keys:
    obj = obj.get(k, []) if isinstance(obj, dict) else []
if isinstance(obj, list):
    for item in obj:
        print(str(item).rstrip('/'))
" 2>/dev/null || true
}

# Build complete list of public directories that need VERSION files.
# This ensures every public directory gets a fresh commit during each release.
build_public_dirs() {
  local -a dirs=()

  # Crate directories
  while IFS= read -r d; do
    [[ -n "$d" ]] && dirs+=("$d")
  done < <(manifest_array "selection.crates.public")

  # Top-level public directories
  while IFS= read -r d; do
    [[ -n "$d" ]] && dirs+=("$d")
  done < <(manifest_array "selection.dirs.public")

  # Cargo config directory
  while IFS= read -r f; do
    [[ -n "$f" ]] && dirs+=("$(dirname "$f")")
  done < <(manifest_array "selection.cargo.public")

  # Doc directories
  while IFS= read -r d; do
    [[ -n "$d" ]] && dirs+=("$d")
  done < <(manifest_array "selection.docs.dirs_public")

  # Script directory (parent of listed scripts)
  while IFS= read -r f; do
    [[ -n "$f" ]] && dirs+=("$(dirname "$f")")
  done < <(manifest_array "selection.scripts.public")

  # Doc files (parent directory)
  while IFS= read -r f; do
    [[ -n "$f" ]] && dirs+=("$(dirname "$f")")
  done < <(manifest_array "selection.docs.files_public")

  # Deduplicate and exclude gitignored directories (e.g. vendor/) where
  # a committed VERSION file would conflict with release-plz's
  # "committed + ignored" check.
  printf '%s\n' "${dirs[@]}" | sort -u | while IFS= read -r d; do
    # Skip directories whose VERSION file would be gitignored
    if git check-ignore -q "$d/VERSION" 2>/dev/null; then
      continue
    fi
    echo "$d"
  done
}

VET_CONFIG="supply-chain/config.toml"
VET_TMP_FILE=""
WORKSPACE_CRATE_NAMES=()
VET_MANAGED_CRATE_NAMES=()

cleanup_temp_files() {
    if [ -n "$VET_TMP_FILE" ]; then
        rm -f "$VET_TMP_FILE"
    fi
}

trap cleanup_temp_files EXIT

# --- Helpers ---

# Check that the canonical version field in a file matches WORKSPACE_VERSION.
# Uses pattern-specific checks rather than substring grep to avoid false positives.
check_version_in_file() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    local current_ver
    current_ver=$(detect_old_version "$file")

    if [ "$file" = ".mcp/server.json" ] && ! grep -q "\"identifier\": \"ghcr.io/verivus-oss/sqry-mcp:v${WORKSPACE_VERSION}\"" "$file"; then
        printf "❌ %-50s identifier tag drift → needs v%s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
        return
    fi

    if [ "$current_ver" = "$WORKSPACE_VERSION" ]; then
        printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
    elif [ -z "$current_ver" ]; then
        printf "❌ %-50s unknown → needs %s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    else
        printf "❌ %-50s %s → needs %s\n" "$label" "$current_ver" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    fi
}

# The MCP registry manifest is kept in two places that must stay byte-identical:
# `.mcp/server.json` (the repo-root discovery copy that this script
# version-syncs) and `sqry-mcp/server.json` (the copy
# scripts/release/register-mcp-registry.sh actually publishes to the registry).
# Publishing a stale/divergent copy is exactly how the live entry drifted to a
# no-packages state, so treat any divergence as an error.
MCP_SERVER_JSON_SRC=".mcp/server.json"
MCP_SERVER_JSON_MIRROR="sqry-mcp/server.json"

check_mcp_server_json_mirror() {
    local label="mirror: ${MCP_SERVER_JSON_MIRROR}"
    if [ ! -f "$MCP_SERVER_JSON_SRC" ] || [ ! -f "$MCP_SERVER_JSON_MIRROR" ]; then
        printf "⚠️  %-50s a server.json copy is missing\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi
    if diff -q "$MCP_SERVER_JSON_SRC" "$MCP_SERVER_JSON_MIRROR" >/dev/null; then
        printf "✅ %-50s in sync with %s\n" "$label" "$MCP_SERVER_JSON_SRC"
    else
        printf "❌ %-50s diverged from %s\n" "$label" "$MCP_SERVER_JSON_SRC"
        ERRORS=$((ERRORS + 1))
    fi
}

# Check package.json: both .version and .binaryVersion must match.
check_package_json() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    local pkg_ver bin_ver
    pkg_ver=$(jq -r '.version' "$file")
    bin_ver=$(jq -r '.binaryVersion // empty' "$file")

    if [ "$pkg_ver" = "$WORKSPACE_VERSION" ] && { [ -z "$bin_ver" ] || [ "$bin_ver" = "$WORKSPACE_VERSION" ]; }; then
        printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
    else
        local detail="version=$pkg_ver"
        if [ -n "$bin_ver" ] && [ "$bin_ver" != "$pkg_ver" ]; then
            detail="version=$pkg_ver, binaryVersion=$bin_ver"
        fi
        printf "❌ %-50s %s → needs %s\n" "$label" "$detail" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    fi
}

fix_version_in_file() {
    local file="$1"
    local old_ver="$2"

    if [ ! -f "$file" ]; then
        return
    fi

    # Replace both bare semver tokens and `v`-prefixed release tags used in OCI identifiers.
    # Keep the match bounded so unrelated substrings are not rewritten.
    local old_ver_escaped="${old_ver//./\\.}"

    sed -i \
        -e "s/\bv${old_ver_escaped}\b/v${WORKSPACE_VERSION}/g" \
        -e "s/\b${old_ver_escaped}\b/${WORKSPACE_VERSION}/g" \
        "$file"
    FIXED=$((FIXED + 1))
}

fix_dates_in_file() {
    local file="$1"

    if [ ! -f "$file" ]; then
        return
    fi

    # Replace **Last Updated**: YYYY-MM-DD patterns
    sed -i "s/\(\*\*Last Updated\*\*:\s*\)[0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}/\1${TODAY}/g" "$file"
}

detect_old_version() {
    local file="$1"
    local old_ver

    # Try markdown **Version**: pattern first (includes **MCP Server Version**: etc.)
    old_ver=$(grep -oP '\*\*(?:[A-Za-z ]*)?Version\*\*:\s*\K[0-9]+\.[0-9]+\.[0-9]+' "$file" 2>/dev/null | head -1 || true)

    # Try YAML version: pattern
    if [ -z "$old_ver" ]; then
        old_ver=$(grep -oP '^version:\s*\K[0-9]+\.[0-9]+\.[0-9]+' "$file" 2>/dev/null | head -1 || true)
    fi

    # Try JSON "version": pattern
    if [ -z "$old_ver" ]; then
        old_ver=$(grep -oP '"version":\s*"\K[0-9]+\.[0-9]+\.[0-9]+' "$file" 2>/dev/null | head -1 || true)
    fi

    echo "$old_ver"
}

load_workspace_crate_names() {
    if [ "${#WORKSPACE_CRATE_NAMES[@]}" -gt 0 ]; then
        return
    fi

    mapfile -t WORKSPACE_CRATE_NAMES < <(
        cargo metadata --no-deps --format-version 1 \
            | jq -r '.packages[] | select(.source == null) | .name' \
            | sort -u
    )
}

crate_has_vet_policy() {
    local crate="$1"

    awk -v crate="$crate" '
        BEGIN {
            target = "[policy." crate "]"
            in_block = 0
            enabled = 0
        }
        $0 == target {
            in_block = 1
            next
        }
        in_block && /^\[/ {
            exit enabled ? 0 : 1
        }
        in_block && $1 == "audit-as-crates-io" && $3 == "true" {
            enabled = 1
            exit 0
        }
        END {
            exit enabled ? 0 : 1
        }
    ' "$VET_CONFIG"
}

load_vet_managed_crates() {
    if [ "${#VET_MANAGED_CRATE_NAMES[@]}" -gt 0 ]; then
        return
    fi

    if [ ! -f "$VET_CONFIG" ]; then
        return
    fi

    load_workspace_crate_names
    local crate
    for crate in "${WORKSPACE_CRATE_NAMES[@]}"; do
        if crate_has_vet_policy "$crate"; then
            VET_MANAGED_CRATE_NAMES+=("$crate")
        fi
    done
}

get_vet_exemption_versions() {
    local crate="$1"

    awk -v crate="$crate" '
        BEGIN {
            target = "[[exemptions." crate "]]"
            in_block = 0
        }
        $0 == target {
            in_block = 1
            next
        }
        in_block && $1 == "version" {
            gsub(/"/, "", $3)
            print $3
            in_block = 0
            next
        }
        in_block && /^\[\[/ {
            in_block = 0
        }
    ' "$VET_CONFIG"
}

check_vet_exemption_for_crate() {
    local crate="$1"
    local label="cargo-vet exemption: ${crate}"

    if [ ! -f "$VET_CONFIG" ]; then
        printf "⚠️  %-50s config file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    local current_versions
    current_versions=$(get_vet_exemption_versions "$crate")

    if [ -z "$current_versions" ]; then
        printf "❌ %-50s missing → needs %s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
        return
    fi

    if printf '%s\n' "$current_versions" | grep -qxF "$WORKSPACE_VERSION" \
        && [ "$(printf '%s\n' "$current_versions" | sort -u | wc -l)" -eq 1 ]; then
        printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
    else
        local summarized_versions
        summarized_versions=$(printf '%s\n' "$current_versions" | sort -u | paste -sd ',' -)
        printf "❌ %-50s %s → needs %s\n" "$label" "$summarized_versions" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
    fi
}

fix_vet_exemption_for_crate() {
    local crate="$1"
    VET_TMP_FILE="${VET_CONFIG}.tmp"

    awk -v crate="$crate" -v new_ver="$WORKSPACE_VERSION" '
        BEGIN {
            target = "[[exemptions." crate "]]"
            in_block = 0
        }
        $0 == target {
            in_block = 1
            print
            next
        }
        in_block && $1 == "version" {
            print "version = \"" new_ver "\""
            in_block = 0
            next
        }
        in_block && /^\[\[/ {
            in_block = 0
        }
        {
            print
        }
    ' "$VET_CONFIG" > "$VET_TMP_FILE"

    mv "$VET_TMP_FILE" "$VET_CONFIG"
    VET_TMP_FILE=""
    FIXED=$((FIXED + 1))
}

check_changelog() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    if grep -q "## \[${WORKSPACE_VERSION}\]" "$file"; then
        printf "✅ %-50s ## [%s] entry exists\n" "$label" "$WORKSPACE_VERSION"
    else
        printf "⚠️  %-50s missing ## [%s] entry\n" "$label" "$WORKSPACE_VERSION"
        WARNINGS=$((WARNINGS + 1))
    fi
}

check_date_only_file() {
    local file="$1"
    local label="${2:-$file}"

    if [ ! -f "$file" ]; then
        printf "⚠️  %-50s file not found\n" "$label"
        WARNINGS=$((WARNINGS + 1))
        return
    fi

    if grep -q '\*\*Last Updated\*\*:[[:space:]]*[0-9]\{4\}-[0-9]\{2\}-[0-9]\{2\}' "$file"; then
        printf "✅ %-50s Last Updated found\n" "$label"
    else
        printf "⚠️  %-50s missing **Last Updated** marker\n" "$label"
        WARNINGS=$((WARNINGS + 1))
    fi
}

# --- Version-only files (Category A) ---
VERSION_ONLY_FILES=(
    "QUICKSTART.md"
    "sqry-cli/README.md"
    ".claude/skills/sqry-semantic-search/SKILL.md"
    ".mcp/server.json"
)

# --- Version + date files (Category B) ---
VERSION_DATE_FILES=(
    "sqry-mcp/README.md"
    "sqry-mcp/USER_GUIDE.md"
    "sqry-mcp/TROUBLESHOOTING.md"
    "sqry-vscode/README.md"
    "sqry-vscode/USER_GUIDE.md"
    "sqry-vscode/TROUBLESHOOTING.md"
    "docs/SCHEMA.md"
    "docs/SEMANTIC_VERSIONING.md"
)

# --- JSON file (special handling) ---
VSCODE_JSON="sqry-vscode/package.json"

# --- Date-only files (no version field expected) ---
DATE_ONLY_FILES=(
    "docs/templates/README.md"
)

# --- Changelog files (check-only) ---
CHANGELOG_FILES=(
    "CHANGELOG.md"
    "sqry-vscode/CHANGELOG.md"
)

echo "--- Version-only files ---"
for f in "${VERSION_ONLY_FILES[@]}"; do
    check_version_in_file "$f"
done

echo ""
echo "--- Version + date files ---"
for f in "${VERSION_DATE_FILES[@]}"; do
    check_version_in_file "$f"
done

echo ""
echo "--- MCP registry manifest mirror ---"
check_mcp_server_json_mirror

echo ""
echo "--- JSON files ---"
check_package_json "$VSCODE_JSON"

echo ""
echo "--- cargo-vet first-party exemptions ---"
load_vet_managed_crates
for crate in "${VET_MANAGED_CRATE_NAMES[@]}"; do
    check_vet_exemption_for_crate "$crate"
done

echo ""
echo "--- Date-only files ---"
for f in "${DATE_ONLY_FILES[@]}"; do
    check_date_only_file "$f"
done

echo ""
echo "--- Changelogs (check-only) ---"
for f in "${CHANGELOG_FILES[@]}"; do
    check_changelog "$f"
done

# --- VERSION file stamps (Category E: ensures every public directory gets a
#     fresh commit during each release so the public repo shows consistent
#     "last commit" metadata across all folders) ---
echo ""
echo "--- VERSION file stamps ---"
VERSION_STAMP_ERRORS=0
mapfile -t PUBLIC_DIRS_LIST < <(build_public_dirs)
for d in "${PUBLIC_DIRS_LIST[@]}"; do
    [[ -d "$d" ]] || continue
    vfile="$d/VERSION"
    label="VERSION: $d/"
    if [ -f "$vfile" ]; then
        file_ver=$(head -1 "$vfile" | tr -d '[:space:]')
        if [ "$file_ver" = "$WORKSPACE_VERSION" ]; then
            printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
        else
            printf "❌ %-50s %s → needs %s\n" "$label" "$file_ver" "$WORKSPACE_VERSION"
            ERRORS=$((ERRORS + 1))
            VERSION_STAMP_ERRORS=$((VERSION_STAMP_ERRORS + 1))
        fi
    else
        printf "❌ %-50s missing → needs %s\n" "$label" "$WORKSPACE_VERSION"
        ERRORS=$((ERRORS + 1))
        VERSION_STAMP_ERRORS=$((VERSION_STAMP_ERRORS + 1))
    fi
done

# --- Sanitization review contract hashes (Category F) ---
echo ""
echo "--- Sanitization review contract hashes ---"
REVIEW_CONTRACT="docs/reviews/release-workflows/sanitization-review-contract.toml"
CONTRACT_HASH_STALE=0
if [ -f "$REVIEW_CONTRACT" ]; then
    while IFS= read -r covered_path; do
        [[ -n "$covered_path" ]] || continue
        label="contract: ${covered_path}"
        if [ ! -f "$covered_path" ]; then
            printf "⚠️  %-50s file not found\n" "$label"
            WARNINGS=$((WARNINGS + 1))
            continue
        fi
        current_hash=$(sha256sum "$covered_path" | cut -d' ' -f1)
        recorded_hash=$(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('${REVIEW_CONTRACT}').read_text())
print(data.get('covered_path_hashes', {}).get('${covered_path}', ''))
" 2>/dev/null || true)
        if [ -z "$recorded_hash" ]; then
            printf "❌ %-50s missing hash\n" "$label"
            ERRORS=$((ERRORS + 1))
            CONTRACT_HASH_STALE=$((CONTRACT_HASH_STALE + 1))
        elif [ "$current_hash" = "$recorded_hash" ]; then
            printf "✅ %-50s %s…\n" "$label" "${current_hash:0:16}"
        else
            printf "❌ %-50s stale → needs %s…\n" "$label" "${current_hash:0:16}"
            ERRORS=$((ERRORS + 1))
            CONTRACT_HASH_STALE=$((CONTRACT_HASH_STALE + 1))
        fi
    done < <(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('${REVIEW_CONTRACT}').read_text())
for p in data.get('covered_path_hashes', {}):
    print(p)
" 2>/dev/null || true)
else
    printf "⚠️  %-50s not found\n" "$REVIEW_CONTRACT"
    WARNINGS=$((WARNINGS + 1))
fi

# --- Fix mode ---
if $FIX_MODE && [ $ERRORS -gt 0 ]; then
    echo ""
    echo "--- Fixing versions ---"

    # Fix version-only files
    for f in "${VERSION_ONLY_FILES[@]}"; do
        if [ -f "$f" ]; then
            old_ver=$(detect_old_version "$f")
            if [ -n "$old_ver" ] && [ "$old_ver" != "$WORKSPACE_VERSION" ]; then
                fix_version_in_file "$f" "$old_ver"
                printf "🔧 %-50s %s → %s\n" "$f" "$old_ver" "$WORKSPACE_VERSION"
            fi
        fi
    done

    # Force the published registry manifest to mirror the version-synced source.
    # Runs after the version-only fixes so .mcp/server.json is already at the
    # workspace version before the copy.
    if [ -f "$MCP_SERVER_JSON_SRC" ] && [ -f "$MCP_SERVER_JSON_MIRROR" ] \
        && ! diff -q "$MCP_SERVER_JSON_SRC" "$MCP_SERVER_JSON_MIRROR" >/dev/null; then
        cp "$MCP_SERVER_JSON_SRC" "$MCP_SERVER_JSON_MIRROR"
        FIXED=$((FIXED + 1))
        printf "🔧 %-50s mirrored from %s\n" "$MCP_SERVER_JSON_MIRROR" "$MCP_SERVER_JSON_SRC"
    fi

    # Fix version+date files
    for f in "${VERSION_DATE_FILES[@]}"; do
        if [ -f "$f" ]; then
            old_ver=$(detect_old_version "$f")
            if [ -n "$old_ver" ] && [ "$old_ver" != "$WORKSPACE_VERSION" ]; then
                fix_version_in_file "$f" "$old_ver"
                printf "🔧 %-50s %s → %s (version)\n" "$f" "$old_ver" "$WORKSPACE_VERSION"
            fi
            fix_dates_in_file "$f"
        fi
    done

    # Fix package.json with jq — always update both version and binaryVersion
    if [ -f "$VSCODE_JSON" ]; then
        local_ver=$(jq -r '.version' "$VSCODE_JSON")
        local_bin_ver=$(jq -r '.binaryVersion // empty' "$VSCODE_JSON")
        if [ "$local_ver" != "$WORKSPACE_VERSION" ] || { [ -n "$local_bin_ver" ] && [ "$local_bin_ver" != "$WORKSPACE_VERSION" ]; }; then
            jq ".version = \"$WORKSPACE_VERSION\" | .binaryVersion = \"$WORKSPACE_VERSION\"" \
                "$VSCODE_JSON" > "$VSCODE_JSON.tmp"
            mv "$VSCODE_JSON.tmp" "$VSCODE_JSON"
            FIXED=$((FIXED + 1))
            printf "🔧 %-50s → %s\n" "$VSCODE_JSON" "$WORKSPACE_VERSION"
        fi
    fi

    # Fix first-party cargo-vet exemption drift without touching third-party entries.
    if [ -f "$VET_CONFIG" ]; then
        for crate in "${VET_MANAGED_CRATE_NAMES[@]}"; do
            current_vet_versions=$(get_vet_exemption_versions "$crate")
            if [ -z "$current_vet_versions" ]; then
                printf "⚠️  %-50s missing block; use cargo vet certify or add the exemption manually\n" "cargo-vet exemption: ${crate}"
                WARNINGS=$((WARNINGS + 1))
                continue
            fi

            if ! printf '%s\n' "$current_vet_versions" | grep -qxF "$WORKSPACE_VERSION" \
                || [ "$(printf '%s\n' "$current_vet_versions" | sort -u | wc -l)" -ne 1 ]; then
                current_vet_summary=$(printf '%s\n' "$current_vet_versions" | sort -u | paste -sd ',' -)
                fix_vet_exemption_for_crate "$crate"
                printf "🔧 %-50s %s → %s\n" "cargo-vet exemption: ${crate}" "$current_vet_summary" "$WORKSPACE_VERSION"
            fi
        done
    fi

    # Fix VERSION file stamps in all public directories
    if [ "$VERSION_STAMP_ERRORS" -gt 0 ]; then
        for d in "${PUBLIC_DIRS_LIST[@]}"; do
            [[ -d "$d" ]] || continue
            vfile="$d/VERSION"
            current_ver=""
            if [ -f "$vfile" ]; then
                current_ver=$(head -1 "$vfile" | tr -d '[:space:]')
            fi
            if [ "$current_ver" != "$WORKSPACE_VERSION" ]; then
                echo "$WORKSPACE_VERSION" > "$vfile"
                FIXED=$((FIXED + 1))
                printf "🔧 %-50s → %s\n" "$vfile" "$WORKSPACE_VERSION"
            fi
        done
    fi

    # Refresh dates in date-only files
    for f in "${DATE_ONLY_FILES[@]}"; do
        if [ -f "$f" ]; then
            fix_dates_in_file "$f"
        fi
    done

    # Recompute sanitization review contract hashes for any covered files
    # that changed. This prevents the check-release-process.sh invariant
    # from failing when a covered file (e.g. release-manifest.toml) is
    # legitimately modified.
    REVIEW_CONTRACT="docs/reviews/release-workflows/sanitization-review-contract.toml"
    if [ -f "$REVIEW_CONTRACT" ]; then
        CONTRACT_HASH_FIXES=0
        while IFS= read -r covered_path; do
            [[ -n "$covered_path" ]] || continue
            [[ -f "$covered_path" ]] || continue
            current_hash=$(sha256sum "$covered_path" | cut -d' ' -f1)
            recorded_hash=$(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('${REVIEW_CONTRACT}').read_text())
print(data.get('covered_path_hashes', {}).get('${covered_path}', ''))
" 2>/dev/null || true)
            if [ -n "$recorded_hash" ] && [ "$current_hash" != "$recorded_hash" ]; then
                # Use python to update the TOML value (sed on TOML hashes is fragile)
                python3 -c "
import pathlib
contract = pathlib.Path('${REVIEW_CONTRACT}')
text = contract.read_text()
text = text.replace('${recorded_hash}', '${current_hash}')
contract.write_text(text)
"
                CONTRACT_HASH_FIXES=$((CONTRACT_HASH_FIXES + 1))
                FIXED=$((FIXED + 1))
                printf "🔧 %-50s hash → %s\n" "contract: ${covered_path}" "${current_hash:0:16}…"
            fi
        done < <(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('${REVIEW_CONTRACT}').read_text())
for p in data.get('covered_path_hashes', {}):
    print(p)
" 2>/dev/null || true)
    fi

    echo ""
    echo "Fixed $FIXED file(s). Re-running check..."
    echo ""

    # Re-check after fix
    ERRORS=0
    WARNINGS=0

    for f in "${VERSION_ONLY_FILES[@]}"; do
        check_version_in_file "$f"
    done
    for f in "${VERSION_DATE_FILES[@]}"; do
        check_version_in_file "$f"
    done
    check_package_json "$VSCODE_JSON"
    for crate in "${VET_MANAGED_CRATE_NAMES[@]}"; do
        check_vet_exemption_for_crate "$crate"
    done
    for f in "${DATE_ONLY_FILES[@]}"; do
        check_date_only_file "$f"
    done
    for f in "${CHANGELOG_FILES[@]}"; do
        check_changelog "$f"
    done
    VERSION_STAMP_ERRORS=0
    for d in "${PUBLIC_DIRS_LIST[@]}"; do
        [[ -d "$d" ]] || continue
        vfile="$d/VERSION"
        label="VERSION: $d/"
        if [ -f "$vfile" ]; then
            file_ver=$(head -1 "$vfile" | tr -d '[:space:]')
            if [ "$file_ver" = "$WORKSPACE_VERSION" ]; then
                printf "✅ %-50s %s\n" "$label" "$WORKSPACE_VERSION"
            else
                printf "❌ %-50s %s → needs %s\n" "$label" "$file_ver" "$WORKSPACE_VERSION"
                ERRORS=$((ERRORS + 1))
            fi
        else
            printf "❌ %-50s missing → needs %s\n" "$label" "$WORKSPACE_VERSION"
            ERRORS=$((ERRORS + 1))
        fi
    done
    # Re-check contract hashes
    if [ -f "$REVIEW_CONTRACT" ]; then
        while IFS= read -r covered_path; do
            [[ -n "$covered_path" ]] || continue
            [[ -f "$covered_path" ]] || continue
            label="contract: ${covered_path}"
            current_hash=$(sha256sum "$covered_path" | cut -d' ' -f1)
            recorded_hash=$(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('${REVIEW_CONTRACT}').read_text())
print(data.get('covered_path_hashes', {}).get('${covered_path}', ''))
" 2>/dev/null || true)
            if [ "$current_hash" = "$recorded_hash" ]; then
                printf "✅ %-50s %s…\n" "$label" "${current_hash:0:16}"
            else
                printf "❌ %-50s stale → needs %s…\n" "$label" "${current_hash:0:16}"
                ERRORS=$((ERRORS + 1))
            fi
        done < <(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('${REVIEW_CONTRACT}').read_text())
for p in data.get('covered_path_hashes', {}):
    print(p)
" 2>/dev/null || true)
    fi
fi

# --- Summary ---
echo ""
if [ $ERRORS -gt 0 ]; then
    echo "$ERRORS version-managed item(s) need update."
    if [ $WARNINGS -gt 0 ]; then
        echo "$WARNINGS changelog(s) need manual entry."
    fi
    if ! $FIX_MODE; then
        echo "Run with --fix to update versions and dates."
    fi
    exit 1
elif [ $WARNINGS -gt 0 ]; then
    echo "All versions in sync. $WARNINGS changelog(s) need manual entry."
    exit 0
else
    echo "All versions and changelogs in sync."
    exit 0
fi
